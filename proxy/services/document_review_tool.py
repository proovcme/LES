"""Request-bound, model-owned review queue for explicitly selected indexed units."""
from __future__ import annotations

import asyncio
from uuid import uuid4

from backend.runtime_paths import mutable_path
from backend.interface import EmbeddingContractError
from proxy.services.document_task_store import digest
from proxy.services.review_task_store import ReviewTaskStore
from proxy.services.review_source_reader import ReviewSourceReader, read_fragment, validate_selector
from proxy.services.tabular_document_service import encode

TOOL_NAME = 'document_review'


class DocumentReviewTool:
    def __init__(self, store, reader, owner, question, max_chars=4000):
        self.store, self.reader, self.owner, self.question = store, reader, owner, question
        self.max_chars = min(4000, max(200, max_chars))
        self.task_id = store.resolve_reference(owner, question)
        self.last_exchange = []
        self.current_read = {}
        self.last_chunks = ()
        self.pending_read = None
        self.delivery_revision = 0
        self.current_state = {'available_reviews': store.recent_reviews(owner), 'accepted': False}

    def context(self):
        return {'document_review': self.current_state, 'current_read': self.current_read,
                'instruction': 'Resume only the review the user requests. The inventory covers selected indexed '
                'text units, not the whole project or visual pages. Conclusions are model-authored, never accepted '
                'by this tool. Never infer completion from chat history: only the current ledger counts establish '
                'saved progress. Use read cursors before record; needs_recheck requires a fresh read and new revision.'}

    def model_context(self):
        return ({'native_tool_exchange': [self.last_exchange]} if self.last_exchange
                else {'required_evidence': [self.context()]})

    async def prepare_context(self):
        if self.task_id:
            await self.refresh(self.task_id)

    async def coverage_notice(self):
        if not self.task_id:
            return ''
        state, _ = await self.refresh(self.task_id)
        if state['needs_recheck']:
            return (f"**Проверка требует обновления: {state['needs_recheck']} из {state['total']} разделов "
                    'нуждаются в перепроверке. Прежние выводы сохранены в истории.**')
        if state['pending']:
            saved = state['total'] - state['pending']
            return (f"**Проверка не завершена: в журнале есть решения для {saved} из {state['total']} разделов.** "
                    'Текст модели ниже не подтверждает завершение проверки.')
        if state['needs_review']:
            return (f"**В журнале остаются вопросы: {state['needs_review']} из {state['total']} разделов "
                    'отмечены как требующие проверки.**')
        return ''

    def remember_validation_failure(self, arguments, result):
        self.remember(arguments, {'status': 'error', 'missing': result.get('missing', []),
                                  'warnings': result.get('warnings', [])})

    def remember(self, arguments, payload):
        call_id = 'review_' + uuid4().hex
        self.last_exchange = [
            {'role': 'assistant', 'content': None, 'tool_calls': [{'id': call_id, 'type': 'function',
                'function': {'name': TOOL_NAME, 'arguments': encode(arguments)}}]},
            {'role': 'tool', 'tool_call_id': call_id, 'name': TOOL_NAME,
             'content': encode({'result': payload, 'document_state': self.context()})}]

    def acknowledge_model_context(self, messages):
        """Only successful inference with the complete native tool message credits a page."""
        if self.pending_read and self.last_exchange and self.last_exchange[-1] in messages:
            task_id, unit_id, snapshot, payload = self.pending_read
            self.store.remember_read(self.owner, task_id, unit_id, snapshot,
                                     payload['fragment_id'], payload['offset'], payload['stop'])
            self.pending_read = None
            self.delivery_revision += 1

    async def snapshots(self, task):
        snapshots, errors, originals = {}, {}, {}
        stamp = self.reader.journal.read_stamp() if self.reader.journal else None
        for unit in task['units']:
            try:
                snapshots[unit['unit_id']] = await self.reader.read(unit['selector'], originals=originals)
            except ValueError as error:
                # Missing/changed originals invalidate coverage without deleting old conclusions.
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
            self.current_state = {'task_id': task_id, 'coverage_complete': False,
                                  'verification_unavailable': True, 'accepted': False}
            raise
        state = self.store.status(self.owner, task_id, {key: value['fingerprint'] for key, value in snapshots.items()})
        state['source_errors'] = errors
        state['blocked_units'] = len(errors)
        state['needs_attention'] = state['needs_attention'] or bool(errors)
        state['next_unit_id'] = next((unit['unit_id'] for name in ('needs_recheck', 'pending')
                                     for unit in state['units'] if unit['state'] == name and unit['unit_id'] in snapshots), None)
        self.current_state = {key: value for key, value in state.items() if key not in {'units', 'source_errors'}}
        if self.task_id != task_id or self.current_read.get('source_fingerprint') not in {
                snapshot['fingerprint'] for snapshot in snapshots.values()}:
            self.current_read = {}
        return state, snapshots

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
        self.pending_read = None
        operation = arguments['operation']
        if operation == 'start':
            selectors = [validate_selector(item, self.reader.allowed) for item in arguments.get('selectors', [])]
            if not 1 <= len(selectors) <= 32:
                raise ValueError('Выберите от 1 до 32 документов или разделов')
            # Fail atomically: do not create a successful inventory of unreadable sources.
            originals = {}
            for selector in selectors:
                await self.reader.read(selector, originals=originals)
            self.task_id = self.store.create(self.owner, self.question, selectors)
            state, _ = await self.refresh(self.task_id)
            return state
        task_id = arguments.get('task_id') or self.task_id
        if operation == 'status' and not task_id:
            return {'available_reviews': self.store.recent_reviews(self.owner), 'accepted': False}
        state, snapshots = await self.refresh(task_id)
        self.task_id = task_id
        if operation == 'status':
            return state
        if operation == 'history':
            return {**self.store.history(self.owner, task_id, arguments.get('offset', 0)),
                    'task_id': task_id, 'historical': True, 'accepted': False}
        unit_id = arguments.get('unit_id') or state['next_unit_id']
        if unit_id not in snapshots:
            raise ValueError('Раздел недоступен или источник изменился; прочитайте status и обновите индекс')
        snapshot = snapshots[unit_id]
        if operation == 'read':
            payload = read_fragment(snapshot, fragment_index=arguments.get('fragment_index', 0),
                                    offset=arguments.get('offset', 0), max_chars=self.max_chars)
            self.pending_read = (task_id, unit_id, snapshot, payload)
            self.current_read = payload
            from backend.interface import Chunk
            fragment = snapshot['fragments'][arguments.get('fragment_index', 0)]
            self.last_chunks = (Chunk(fragment['text'], str(fragment['locator'].get('doc_id') or ''),
                snapshot['selector']['file_name'], 0.0, {**fragment['locator'],
                'dataset_id': snapshot['selector']['dataset_id'], 'qdrant_point_id': fragment['id']}),)
            return {'task_id': task_id, 'revision': state['revision'], 'unit_id': unit_id, **payload}
        if operation == 'record':
            dependencies = arguments.get('dependencies', [])
            states = {unit['unit_id']: unit['state'] for unit in state['units']}
            if not isinstance(dependencies, list) or any(states.get(dep) != 'reviewed' for dep in dependencies):
                raise ValueError('Зависимые выводы отсутствуют или нуждаются в перепроверке')
            revision = self.store.record(self.owner, task_id, request_id=arguments.get('request_id'),
                expected_revision=arguments.get('expected_revision'), unit_id=unit_id, snapshot=snapshot,
                source_fingerprint=arguments.get('source_fingerprint'), status=arguments.get('status'),
                note=arguments.get('note'), evidence=arguments.get('evidence'), dependencies=dependencies)
            state, _ = await self.refresh(task_id)
            return {**state, 'saved_revision': revision}
        raise ValueError('Неизвестное действие проверки документов')

    def register(self, harness):
        from proxy.services.tool_contract_service import ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy
        from proxy.services.tool_registry_service import ToolRegistration
        from proxy.services.tool_harness_service import _result
        string = {'type': 'string', 'maxLength': 1000}
        contract = ToolContract(
            name=TOOL_NAME, version='1.0.0', title='Продолжить проверку документов', category='source',
            summary='Persistent review of an EXPLICIT inventory of documents/parent sections in selected datasets. '
                'start uses the current user question as goal and selectors from search provenance. '
                'status lists prior reviews; task_id resumes one. read returns exact indexed text with next_cursor '
                '(fragment_index, offset); follow every cursor before marking a unit reviewed. '
                'record requires unit_id, current expected_revision, source_fingerprint from read, unique request_id, '
                'status, note and exact evidence quotes. dependencies names other reviewed unit IDs whose conclusions '
                'were used. Changed source bytes, indexed text or dependency revisions cause needs_recheck. '
                'history preserves old revisions. accepted is always false. Coverage is selected indexed text only; '
                'never claim a complete project audit or visual inspection.',
            input_schema={'type': 'object', 'properties': {
                'operation': {'type': 'string', 'enum': ['start', 'status', 'read', 'record', 'history']},
                'task_id': string, 'unit_id': string, 'request_id': {'type': 'string', 'minLength': 1, 'maxLength': 120},
                'expected_revision': {'type': 'integer', 'minimum': 0}, 'source_fingerprint': string,
                'fragment_index': {'type': 'integer', 'minimum': 0}, 'offset': {'type': 'integer', 'minimum': 0},
                'selectors': {'type': 'array', 'minItems': 1, 'maxItems': 32, 'items': {
                    'type': 'object', 'properties': {'dataset_id': string, 'file_name': string, 'parent_id': string},
                    'required': ['dataset_id', 'file_name'], 'additionalProperties': False}},
                'status': {'type': 'string', 'enum': ['reviewed', 'needs_review', 'excluded']},
                'note': {'type': 'string', 'minLength': 1, 'maxLength': 1200},
                'dependencies': {'type': 'array', 'maxItems': 16, 'uniqueItems': True, 'items': string},
                'evidence': {'type': 'array', 'minItems': 1, 'maxItems': 4, 'items': {'type': 'object',
                    'properties': {'fragment_id': string, 'quote': {'type': 'string', 'minLength': 1, 'maxLength': 300}},
                    'required': ['fragment_id', 'quote'], 'additionalProperties': False}}},
                'required': ['operation'], 'additionalProperties': False},
            result_schema='les_tool_result_v1', effect=EffectClass.DRAFT, scopes=(), timeout_seconds=30,
            retry=RetryPolicy.NEVER, idempotency=IdempotencyPolicy.NONE,
            result_budget=ResultBudget(max_chars=60000, max_items=4096), model_owned_fields=('note', 'status', 'evidence'),
            provenance='Selected original SHA256, indexed unit fingerprints and immutable model conclusion revisions',
            tags=('document', 'review', 'resume'))

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
