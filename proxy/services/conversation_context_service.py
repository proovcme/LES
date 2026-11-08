"""Compatibility codecs; Rust owns conversation memory, model work and its queue."""
from backend.rag_config import rag_meta_db_path
from backend.workspace_native import catalog_call


class ContextConflict(ValueError):
    pass


def get_context(session_id: str) -> dict:
    return catalog_call(rag_meta_db_path(), 'context_get', session_id=session_id)


def update_context(session_id: str, *, expected_revision: int, **changes) -> dict:
    from proxy.services.chat_session_service import SessionConflictError
    try:
        return catalog_call(rag_meta_db_path(), 'context_update', session_id=session_id,
                            expected_revision=expected_revision, changes=changes)
    except SessionConflictError as error:
        raise ContextConflict(str(error)) from error


def history_turns(session_id: str, *, after_id=0) -> list[dict]:
    return catalog_call(rag_meta_db_path(), 'context_history', session_id=session_id, after_id=after_id)


def prompt_cutoff(session_id: str) -> int | None:
    state = get_context(session_id)
    return int(state['cutoff_id']) if state['enabled'] else None


def context_status(session_id: str) -> dict:
    return catalog_call(rag_meta_db_path(), 'context_status', session_id=session_id)


def forget_context(session_id: str, *, expected_revision: int) -> dict:
    from proxy.services.chat_session_service import SessionConflictError
    try:
        return catalog_call(rag_meta_db_path(), 'context_forget', session_id=session_id, expected_revision=expected_revision)
    except SessionConflictError as error:
        raise ContextConflict(str(error)) from error


def summary_record(session_id: str) -> dict | None:
    state = get_context(session_id)
    if not state['enabled'] or not state['summary']:
        return None
    return {'text': state['summary'], 'source': 'model_summary', 'is_evidence': False,
            'through_history_id': state['through_id'], 'revision': state['revision']}


def _summary(action, **arguments):
    from backend import native_core
    try:
        return native_core.required('conversation_summary', request={'action': action, **arguments})['value']
    except native_core.NativeCoreError as error:
        message = ('Сообщение слишком длинное для выбранной модели. Сохраните важное отдельной записью памяти.'
                   if 'SUMMARY_TURN_TOO_LARGE' in str(error) else
                   'Модель не завершила краткую сводку. История сохранена; попробуйте ещё раз.')
        raise ValueError(message) from error


def summary_batch(turns: list[dict], previous: str, *, max_chars: int) -> tuple[str, int]:
    result = _summary('batch', turns=turns, previous=previous, max_chars=max_chars)
    return result['text'], result['through_id']


def selected_summary(response: str, records: list[str], *, finish_reason: str = '') -> str:
    return _summary('select', response=response, records=records, finish_reason=finish_reason)['summary']


async def summarize(session_id: str, *, force=False, expected_revision: int | None = None) -> dict:
    import httpx
    from proxy.services.local_model_http import context_request
    arguments = {'session_id': session_id, 'force': force}
    if expected_revision is not None:
        arguments['expected_revision'] = expected_revision
    try:
        return await context_request('summarize', **arguments)
    except httpx.HTTPStatusError as error:
        if error.response.status_code == 409:
            raise ContextConflict('Память изменилась. Обновите окно.') from error
        raise
