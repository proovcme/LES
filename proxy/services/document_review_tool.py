"""Classic IO codec; review transitions, coverage and revisions are owned by Rust."""
from __future__ import annotations

import asyncio
from uuid import uuid4

from backend.runtime_paths import mutable_path
from backend.interface import EmbeddingContractError
from proxy.services.document_task_store import digest
from proxy.services.review_task_store import ReviewTaskStore
from proxy.services.review_source_reader import ReviewSourceReader
from proxy.services.tabular_document_service import encode

TOOL_NAME = 'document_review'


class DocumentReviewTool:
    def __init__(self, store, reader, owner, question, max_chars=4000):
        self.store, self.reader, self.owner, self.question = store, reader, owner, question
        self._state = dict(owner=owner, question=question, max_chars=min(4000, max(200, max_chars)),
            # Receipt-only adapters grant no new source selection.
            task_id=store.resolve_reference(owner, question), allowed=sorted(getattr(reader, 'allowed', ())),
            files=sorted(getattr(reader, 'files', ())),
            current_read={}, pending_read=None, delivery_revision=0,
            current_state={'available_reviews': store.recent_reviews(owner), 'accepted': False})
        self.last_exchange, self.last_chunks = [], ()

    @property
    def task_id(self):
        return self._state['task_id']

    @property
    def current_read(self):
        return self._state['current_read']

    @property
    def current_state(self):
        return self._state['current_state']

    @property
    def delivery_revision(self):
        return self._state['delivery_revision']

    def context(self):
        return self.store._call('context', state=self._state)

    def model_context(self):
        return ({'native_tool_exchange': [self.last_exchange]} if self.last_exchange
                else {'required_evidence': [self.context()]})

    async def prepare_context(self):
        if self.task_id:
            await self.refresh(self.task_id)

    async def coverage_notice(self):
        receipt = await self.completion_receipt()
        return receipt['notice'] if receipt and receipt['status'] != 'recorded' else ''

    def remember_validation_failure(self, arguments, result):
        self.remember(arguments, {'status': 'error', 'missing': result.get('missing', []),
                                  'warnings': result.get('warnings', [])})

    async def completion_receipt(self):
        if not self.task_id:
            return None
        from proxy.services.execution_receipt_service import ledger_receipt
        state, _ = await self.refresh(self.task_id)
        return ledger_receipt(TOOL_NAME, state)

    def remember(self, arguments, payload):
        call_id = 'review_' + uuid4().hex
        self.last_exchange = [
            {'role': 'assistant', 'content': None, 'tool_calls': [{'id': call_id, 'type': 'function',
                'function': {'name': TOOL_NAME, 'arguments': encode(arguments)}}]},
            {'role': 'tool', 'tool_call_id': call_id, 'name': TOOL_NAME,
             'content': encode({'result': payload, 'document_state': self.context()})}]

    def acknowledge_model_context(self, messages):
        self._state = self.store._call('ack', state=self._state, messages=messages)

    async def snapshots(self, task):
        snapshots, errors, originals = {}, {}, {}
        stamp = self.reader.journal.read_stamp() if self.reader.journal else None
        for unit in task['units']:
            try:
                snapshots[unit['unit_id']] = await self.reader.read(unit['selector'], originals=originals)
            except ValueError as error:
                errors[unit['unit_id']] = str(error)
            except OSError:
                errors[unit['unit_id']] = 'Оригинал недоступен: файл отсутствует или нет прав чтения'
        if self.reader.journal:
            self.reader.journal.assert_unchanged(stamp)
        return snapshots, errors

    async def refresh(self, task_id):
        task = self.store.task(self.owner, task_id)
        try:
            snapshots, errors = await self.snapshots(task)
        except EmbeddingContractError:
            self._state['current_state'] = {'task_id': task_id, 'coverage_complete': False,
                                           'verification_unavailable': True, 'accepted': False}
            raise
        result = await asyncio.to_thread(self.store._call, 'refresh', state=self._state,
                                         task_id=task_id, snapshots=snapshots, errors=errors)
        self._state = result['state']
        return result['payload'], snapshots

    async def assert_for_inference(self):
        if not self.current_read:
            return
        from proxy.services.chat_section_context_service import SectionReadError
        try:
            snapshot = await self.reader.read(self.current_read['selector'])
            if snapshot['fingerprint'] != self.current_read['source_fingerprint']:
                raise ValueError('Источник проверки изменился')
        except (ValueError, OSError, EmbeddingContractError) as error:
            raise SectionReadError(409, detail={'code': 'INDEX_CHANGED_DURING_SEARCH'}) from error

    async def execute(self, arguments):
        self.last_chunks = ()
        plan = self.store._call('plan', state=self._state, arguments=arguments)
        snapshots, errors = await self.snapshots({'units': [
            {'unit_id': digest(selector), 'selector': selector} for selector in plan['selectors']]})
        result = await asyncio.to_thread(self.store._call, 'execute', state=self._state,
                                         arguments=arguments, snapshots=snapshots, errors=errors)
        self._state = result['state']
        fragment = result.get('chunk')
        if fragment:
            from backend.interface import Chunk
            selector = result['payload']['selector']
            self.last_chunks = (Chunk(fragment['text'], str(fragment['locator'].get('doc_id') or ''),
                selector['file_name'], 0.0, {**fragment['locator'],
                'dataset_id': selector['dataset_id'], 'qdrant_point_id': fragment['id']}),)
        return result['payload']

    def register(self, harness):
        from proxy.services.tool_contract_service import ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy
        from proxy.services.tool_registry_service import ToolRegistration
        from proxy.services.tool_harness_service import _result
        fields = self.store._call('contract')
        fields['effect'] = EffectClass(fields['effect'])
        fields['retry'] = RetryPolicy(fields['retry'])
        fields['idempotency'] = IdempotencyPolicy(fields['idempotency'])
        fields['result_budget'] = ResultBudget(**fields['result_budget'])
        contract = ToolContract(**fields)

        async def handler(arguments):
            try:
                payload = await self.execute(arguments)
            except Exception:
                self.remember(arguments, {'status': 'error', 'message': 'Проверка не сохранена; запросите status'})
                raise
            self.remember(arguments, payload)
            return _result(tool=TOOL_NAME, operation=arguments['operation'], inputs=[], status='ok',
                           result=payload, trace='scoped_review_inventory')
        harness._registry.register(ToolRegistration(contract=contract, handler=handler))
        return contract.public_payload()


def bind_review_tool(req, backend, *, dataset_ids, profile_revision, model_revision, input_budget, file_names=(), available_slots=1):
    from backend.product_edition import is_light
    if not is_light() or not dataset_ids or not req.session_id or available_slots < 1:
        return None
    owner = digest({'session': req.session_id, 'project': getattr(req, 'project_id', None),
                    'datasets': sorted(str(item) for item in dataset_ids), 'scope': getattr(req, 'scope', None),
                    'profile': profile_revision, 'model': model_revision, 'files': sorted(file_names)})
    return DocumentReviewTool(ReviewTaskStore(mutable_path('data/document-tasks.sqlite3')),
                              ReviewSourceReader(backend, dataset_ids, file_names), owner, req.question, input_budget // 8)
