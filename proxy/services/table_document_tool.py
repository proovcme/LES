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
        """Only the current page is model-visible; saved pages stay in SQLite."""
        state = self.state()
        matches = self.store.matches_question(self.owner, self.task_id, self.question)
        next_action = ('record' if self.current.get('packet_id') and self.current.get('rows')
                       else 'next' if state['pending'] else 'finish_or_read_results')
        return {'table_task': state, 'current_read': self.current,
                'task_is_for_current_question': matches,
                'next_action_for_active_task': next_action,
                'instruction': 'Use table_document for exact cells and saved results. Never infer '
                'completion from a summary. coverage_complete only counts row decisions; it does '
                'not certify their correctness. If task_is_for_current_question is true, the task '
                'is ALREADY started: do not call start again. Read with next, decide every row, '
                'save with record, then next until pending is zero. Continue a prior task only '
                'when the user asks to continue it; for a different objective call start once.'}

    def execute(self, arguments):
        task_id = arguments.get('task_id') or self.task_id
        self.store.status(self.owner, task_id)  # Check binding before switching or disclosing data.
        self.store.activate(self.owner, task_id)
        self.task_id = task_id
        operation = arguments['operation']
        if operation == 'start':
            if self.store.matches_question(self.owner, task_id, self.question):
                action = 'record' if self.current.get('packet_id') and self.current.get('rows') else 'next'
                raise ValueError(f'Task already started for this question. Do not call start again. '
                                 f'Use {action}; current packet and saved results are preserved.')
            self.task_id = self.store.new_question(self.owner, task_id, self.question)
            payload = {'started_for_current_question': True}
        elif operation == 'status':
            payload = {'recent_tasks': self.store.recent(self.owner)}
        elif operation == 'next':
            payload = self.store.next_packet(self.owner, task_id, max_bytes=self.packet_bytes)
        elif operation == 'record':
            self.store.commit(self.owner, task_id, arguments.get('packet_id') or self.current.get('packet_id'),
                              arguments.get('decisions'))
            payload = {'saved': True}
        elif operation == 'results':
            payload = self.store.results(self.owner, task_id, offset=arguments.get('offset', 0),
                                         max_bytes=self.packet_bytes)
        else:
            raise ValueError('Неизвестное действие с таблицей')
        self.current = payload
        return {**self.state(), **payload}

    def register(self, harness):
        from proxy.services.tool_contract_service import (
            ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy)
        from proxy.services.tool_registry_service import ToolRegistration
        from proxy.services.tool_harness_service import _result
        contract = ToolContract(
            name=TOOL_NAME, version='1.0.0', title='Чтение и проверка таблицы', category='attachment',
            summary='Read the attached table in bounded whole-row packets. start creates or resumes '
                'a separate task for the CURRENT user question, with no inherited decisions. '
                'Use start for a new objective; do not use it for continuation. status lists tasks in '
                'this conversation/source/scope; optional task_id resumes one explicitly. next returns '
                'literal cells and packet_id; record saves one decision for EVERY row in that packet '
                '(packet_id may be omitted for the currently open packet). '
                '(including headers: exclude with explanation). Never change source values. '
                'Use reviewed, needs_review or excluded with a note and exact cell coordinates. '
                'A row with missing formula cache or error needs_review. Results survive restart. '
                'results reads saved sources/decisions by offset. Numbers are decimal strings. '
                'A blocked oversized row remains pending; never claim the whole task is finished.',
            input_schema={'type': 'object', 'properties': {
                'operation': {'type': 'string', 'enum': ['start', 'status', 'next', 'record', 'results']},
                'task_id': {'type': 'string'}, 'packet_id': {'type': 'string'},
                'offset': {'type': 'integer', 'minimum': 0},
                'decisions': {'type': 'array', 'maxItems': 12, 'items': {
                    'type': 'object', 'properties': {
                        'row_id': {'type': 'string'},
                        'status': {'type': 'string', 'enum': ['reviewed', 'needs_review', 'excluded']},
                        'note': {'type': 'string', 'minLength': 1, 'maxLength': 1200},
                        'cells': {'type': 'array', 'minItems': 1,
                                  'description': 'Exact cell COORDINATES from the row, e.g. A1, B1. Never cell values or text.',
                                  'items': {'type': 'string', 'pattern': '^[A-Z]{1,3}[1-9][0-9]*$'}}},
                    'required': ['row_id', 'status', 'note', 'cells'], 'additionalProperties': False}}},
                'required': ['operation'], 'additionalProperties': False},
            result_schema='les_tool_result_v1', effect=EffectClass.DRAFT, scopes=(),
            timeout_seconds=20, retry=RetryPolicy.NEVER, idempotency=IdempotencyPolicy.NONE,
            result_budget=ResultBudget(max_chars=18000, max_items=1024), model_owned_fields=('decisions',),
            provenance='Immutable attached table hash and literal coordinates; scoped persistent row decisions',
            tags=('attachment',))

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
    return [item for item in results if item.get('tool') != TOOL_NAME or item.get('status') != 'ok']
