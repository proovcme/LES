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


def register_tools(registry):
    from proxy.services.tool_contract_service import (
        ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy)
    from proxy.services.tool_registry_service import ToolRegistration
    offset = {'type': 'integer', 'minimum': 0}
    definitions = [
        ('list_installed_skills', 'Библиотека навыков',
         'List installed skill descriptions. Continue with next_offset until null.',
         {'offset': offset}, [], lambda args: catalog(**args)),
        ('read_installed_skill', 'Прочитать навык',
         'Read SKILL.md or a referenced text file in the installed skill library. '
         'Follow next_offset until null to read the entire file. Scripts are readable, not executable.',
         {'skill': {'type': 'string'}, 'path': {'type': 'string'}, 'offset': offset},
         ['skill'], lambda args: read(**args)),
    ]
    for name, title, description, properties, required, action in definitions:
        contract = ToolContract(
            name=name, version='1.0.0', title=title, category='skills', summary=description,
            input_schema={'type': 'object', 'properties': properties, 'required': required,
                          'additionalProperties': False}, result_schema='les_tool_result_v1',
            effect=EffectClass.READ, scopes=('skills',), timeout_seconds=5,
            retry=RetryPolicy.SAFE, idempotency=IdempotencyPolicy.NONE,
            result_budget=ResultBudget(max_chars=16000, max_items=12), model_owned_fields=(),
            provenance='User-installed skill library', tags=('skills',))

        def handler(arguments, execute=action, tool_name=name):
            from proxy.services.tool_harness_service import _result
            return _result(tool=tool_name, operation='read_skill', inputs=[], status='ok',
                           result=execute(arguments), trace='installed_skill_library')

        registry.register(ToolRegistration(contract=contract, handler=handler))
