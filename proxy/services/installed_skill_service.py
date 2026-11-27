"""Compatibility codec for the native installed-skill library owner."""
from backend import native_core
from backend.runtime_paths import mutable_path


def library_root():
    return mutable_path('storage/skills').resolve()


def _native(action, **fields):
    response = native_core.required('skills', request={
        'schema': 'les.skills.v1', 'action': action,
        'library': str(library_root().resolve()), **fields})
    if response.get('ok') is not True:
        raise ValueError(response.get('error', {}).get('message', 'Не удалось прочитать навык'))
    return response['value']


def catalog(offset=0):
    return _native('catalog', offset=max(0, int(offset)))


def read(skill, path='SKILL.md', offset=0):
    return _native('read', skill=skill, path=path, offset=max(0, int(offset)))
