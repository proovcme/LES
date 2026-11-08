"""Compatibility framing. Immutable profile storage and decisions are owned by Rust."""
import json
from pathlib import Path
from backend import native_core
from backend.rag_config import rag_meta_db_path

SCHEMA = 'les.chat_profiles.v1'
PROFILE_PROMPT_MAX_CHARS = 16000
PROFILE_SKILL_MAX_CHARS = 8000
PROFILE_MODES = ('search', 'agent')
MODE_LABELS = {'search':'Поиск', 'agent':'Агент'}
DEFAULT_RETRIEVAL_POLICY = {'retrieval_candidate_k':64, 'document_diversity_k':2, 'model_evidence_k':6}


def _factory_contracts():
    return json.loads((Path(__file__).resolve().parents[2]/'config/chat-profile-defaults.json').read_text(encoding='utf-8'))


def _unwrap(result):
    if result.get('status') == 'completed':
        return result['value']
    if result.get('code') == 'PROFILE_INVALID':
        raise ValueError(result.get('detail') or 'Некорректная редакция профиля')
    raise RuntimeError(result.get('code') or 'PROFILE_STORAGE_UNAVAILABLE')


def _pure(action, **arguments):
    return _unwrap(native_core.required('chat_profile', request={
        'schema':'les.chat-profiles.v1','action':action,'arguments':arguments}))


def _call(action, db_path=None, **arguments):
    request = {'schema':'les.chat-profiles.v1','action':action,'arguments':arguments,
               'database':str(Path(db_path if db_path is not None else rag_meta_db_path()).resolve()),
               'factory':_factory_contracts()}
    if action == 'publish_profile':
        request['known_tools'] = sorted(_tool_names())
    return _unwrap(native_core.required('chat_profile', request=request))


def _tool_names():
    from proxy.services.tool_harness_service import harness
    return {str(item['name']) for item in harness().registry()['tools']}


def canonical_profile_mode(mode):
    return _pure('mode', mode=str(mode or ''))


def effective_retrieval_policy(snapshot):
    return _pure('retrieval_policy', snapshot=snapshot or {})


def effective_profile_snapshot(snapshot):
    return _pure('effective', snapshot=snapshot)


def validate_profile_text(kind, text):
    return _pure('validate_text', kind=kind, text=str(text or ''))


def resolve_profile_system_dataset_ids(profile_snapshot, *, current_dataset_ids=None, module_resolver=None):
    return [str(item) for item in (current_dataset_ids or ()) if str(item)]


def publish_text_revision(kind, *, name, text, source_revision_id=None, db_path=None):
    return _call('publish_text', db_path, kind=kind, name=str(name or ''), text=str(text or ''),
                 source_revision_id=source_revision_id)


def publish_profile_revision(*, mode, name, prompt_revision_id, skill_revision_id, tools,
                             model_policy, rag_policy, source_revision_id=None, db_path=None,
                             additional_skill_revision_ids=None):
    return _call('publish_profile', db_path, mode=mode, name=str(name or ''),
        prompt_revision_id=prompt_revision_id, skill_revision_id=skill_revision_id,
        tools=tools, model_policy=model_policy or {}, rag_policy=rag_policy or {},
        source_revision_id=source_revision_id,
        additional_skill_revision_ids=additional_skill_revision_ids or [])


def activate_profile_revision(mode, revision_id, *, db_path=None):
    return _call('activate', db_path, mode=mode, revision_id=revision_id)


def resolve_chat_profile(*, session_id, requested_mode, requested_revision_id=None,
                         apply_revision=False, db_path=None):
    return _call('resolve', db_path, session_id=session_id, requested_mode=requested_mode,
        requested_revision_id=requested_revision_id, apply_revision=apply_revision)


def profile_snapshot(revision_id, *, db_path=None):
    return _call('snapshot', db_path, revision_id=revision_id)


def delete_revision(kind, revision_id, *, db_path=None):
    return _call('delete', db_path, kind=kind, revision_id=revision_id)


def registry_snapshot(*, db_path=None):
    from proxy.services.tool_harness_service import harness
    value = _call('registry', db_path)
    value['tools'] = harness().registry()['tools']
    return value


def import_legacy_prompt_overrides(*, overrides_path=None, db_path=None):
    return {'status':'separate_light_profiles','imported_modes':[]}
