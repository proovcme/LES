"""One request-bound tool for literal table reading and resumable row work."""
from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass, field
from uuid import uuid4

from backend.runtime_paths import mutable_path
from proxy.services.document_task_store import DocumentTaskStore, digest
from proxy.services.tabular_document_service import TABLE_SUFFIXES, read_table, encode

TOOL_NAME = 'table_document'


@dataclass
class TableDocumentTool:
    store: DocumentTaskStore
    owner: str
    task_id: str
    document: dict
    packet_bytes: int
    question: str = ''
    current: dict = field(default_factory=dict)
    last_exchange: list = field(default_factory=list)
    work_requested: bool = False

    def model_context(self):
        return ({'native_tool_exchange': [self.last_exchange]} if self.last_exchange
                else {'required_evidence': [self.context()]})

    def remember_validation_failure(self, arguments, result):
        """Schema/permission failures happen before the registered handler runs."""
        call_id = 'table_' + uuid4().hex
        self.last_exchange = [
            {'role': 'assistant', 'content': None, 'tool_calls': [
                {'id': call_id, 'type': 'function', 'function': {
                    'name': TOOL_NAME, 'arguments': encode(arguments)}}]},
            {'role': 'tool', 'tool_call_id': call_id, 'name': TOOL_NAME,
             'content': encode({'status': 'error', 'missing': result.get('missing', []),
                                'warnings': result.get('warnings', []),
                                'execution': result.get('execution', {}), 'document_state': self.context()})}]

    def state(self):
        return self.store.status(self.owner, self.task_id)

    def coverage_notice(self):
        state = self.state()
        if state['pending']:
            saved = state['total'] - state['pending']
            return (f"**Проверка таблицы не завершена: в журнале сохранено {saved} из {state['total']} строк.** "
                    'Ниже — ответ модели; он не подтверждает завершение проверки.')
        return ''

    def context(self):
        return self.store._call('bound_context', state=self._native_state())

    def completion_receipt(self):
        if not self.work_requested:
            return None  # Reading a cell does not request a review of the whole table.
        from proxy.services.execution_receipt_service import ledger_receipt
        return ledger_receipt(TOOL_NAME, self.state())

    def _native_state(self):
        return dict(owner=self.owner, task_id=self.task_id, question=self.question,
                    current=self.current, packet_bytes=self.packet_bytes, work_requested=self.work_requested)

    def execute(self, arguments):
        if arguments.get('operation') in {'start', 'next', 'record'}:
            self.work_requested = True
        response = self.store._call('bound_execute', state=self._native_state(), arguments=arguments)
        state = response['state']
        self.task_id, self.current, self.work_requested = state['task_id'], state['current'], state['work_requested']
        return response['payload']

    def register(self, harness):
        from proxy.services.tool_contract_service import (
            ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy)
        from proxy.services.tool_registry_service import ToolRegistration
        from proxy.services.tool_harness_service import _result
        from backend import native_core
        fields = native_core.required('document_tasks', request={
            'schema': 'les.document-tasks.v1', 'action': 'contract'})['value']
        fields['effect'] = EffectClass(fields['effect'])
        fields['retry'] = RetryPolicy(fields['retry'])
        fields['idempotency'] = IdempotencyPolicy(fields['idempotency'])
        fields['result_budget'] = ResultBudget(**fields['result_budget'])
        contract = ToolContract(**fields)

        async def handler(arguments):
            call_id = 'table_' + uuid4().hex
            error = None
            try:
                payload = await asyncio.to_thread(self.execute, arguments)
            except Exception as exc:
                error = str(exc)
                raise
            finally:
                self.last_exchange = [
                    {'role': 'assistant', 'content': None, 'tool_calls': [
                        {'id': call_id, 'type': 'function', 'function': {
                            'name': TOOL_NAME, 'arguments': encode(arguments)}}]},
                    {'role': 'tool', 'tool_call_id': call_id, 'name': TOOL_NAME,
                     'content': encode({'status': 'error' if error else 'ok', 'error': error,
                                        'document_state': self.context()})}]
            return _result(tool=TOOL_NAME, operation=arguments['operation'], inputs=[], status='ok',
                           result=payload, trace='scoped_document_task')

        registration = ToolRegistration(contract=contract, handler=handler)
        harness._registry.register(registration)
        return contract.public_payload()


def bind_table_tool(req, *, dataset_ids, profile_revision, model_revision, input_budget):
    """Only application-owned request state can grant access, never tool arguments."""
    from backend.product_edition import is_light
    from proxy.services.chat_attachment_service import resolve_read_attachment
    if not is_light():
        return None
    scope = {'session': req.session_id or uuid4().hex, 'project': getattr(req, 'project_id', None),
             'datasets': sorted(str(item) for item in dataset_ids),
             'scope': getattr(req, 'scope', None), 'profile': profile_revision,
             'model': model_revision}
    scope_key = digest(scope)
    db_path = mutable_path('data/document-tasks.sqlite3')
    if not getattr(req, 'attachment_id', None):
        if not req.session_id or not db_path.exists():
            return None
        store = DocumentTaskStore(db_path)
        saved = store.latest(scope_key)
        if not saved:
            return None
        # A prior task is available for an explicit model read/resume, never replayed.
        document, owner, task_id = json.loads(saved['source']), saved['owner'], saved['id']
    else:
        path, metadata = resolve_read_attachment(req.attachment_id)
        if path.suffix.lower() not in TABLE_SUFFIXES:
            return None
        document = read_table(path, metadata['original_name'])
        if document['sha256'] != metadata['sha256']:
            raise ValueError('Вложение изменилось после проверки')
        owner = digest([scope_key, document['sha256'], document['schema']])
        store = DocumentTaskStore(db_path)
        task_id = store.open(owner, req.question, document, scope_key=scope_key)
    return TableDocumentTool(store, owner, task_id, document,
                             min(6000, max(512, int(input_budget) // 4)), question=req.question)


def manifest(tool):
    return encode({'name': tool.document['name'], 'source_sha256': tool.document['sha256'],
                   'sheets': tool.document['sheets'], 'total_nonempty_rows': tool.document['row_count'],
                   'read': 'table_document', 'cell_policy': tool.document['cell_policy']})


def without_table_replays(results):
    """A growing tool log is trace data; table state has its own bounded context."""
    return [item for item in results if item.get('tool') not in {TOOL_NAME, 'document_review'} or item.get('status') != 'ok']
