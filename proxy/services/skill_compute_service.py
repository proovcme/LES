"""Compatibility codec for native approved-skill scheduling and persistence."""
import asyncio
import os
from pathlib import Path
import sys

from backend import native_core
from backend.runtime_paths import mutable_path
from proxy.services.installed_skill_service import library_root


class SkillExecutionError(ValueError):
    def __init__(self,code,message):
        self.code=code
        super().__init__(message)


def _program():
    explicit=os.getenv('LES_SKILL_PYTHON')
    if explicit:
        return str(Path(explicit).resolve())
    if not native_core.supports('skill_runtime'):
        raise SkillExecutionError('SKILL_RUNTIME_COMPONENT_MISSING',
            'Компонент внешних навыков устарел. Восстановите установку Леса.')
    response=native_core.required('skills',request={
        'schema':'les.skills.v1','action':'runtime',
        'root':str(Path(__file__).resolve().parents[2]),
        'application':str(Path(sys.executable).resolve())})
    if response.get('ok') is not True:
        error=response.get('error',{})
        raise SkillExecutionError(error.get('code','SKILL_RUNTIME_INVALID'),
                                  error.get('message','Не удалось выбрать исполнитель внешних навыков'))
    return response['value']['program']


def _native(action, **fields):
    if action in {'calculate','resume'}:
        fields['program']=_program()
    response = native_core.required('skills', request={
        'schema': 'les.skills.v1', 'action': action,
        'library': str(library_root().resolve()),
        'artifacts': str(mutable_path('artifacts/skill-compute').resolve()),
        **fields})
    if response.get('ok') is not True:
        error=response.get('error',{})
        raise SkillExecutionError(error.get('code','SKILL_EXECUTION_FAILED'),error.get('message','Не удалось выполнить навык'))
    return response['value']


def approved_scripts():
    return _native('approvals')['scripts']


def calculate(skill, path, items):
    return _native('calculate', skill=skill, path=path, items=items)


def read_job(job_id, offset=0):
    return _native('read_job', job_id=job_id, offset=max(0, int(offset)))


def register_tools(registry):
    from proxy.services.tool_contract_service import (
        ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy)
    from proxy.services.tool_registry_service import ToolRegistration
    definitions = [
        ('run_skill_calculation', 'Вычисление по навыку',
         'Run an approved pure Python skill function on JSON inputs with unique row IDs. '
         'Persist each row, return job_id and pending_ids; this does not verify input facts. '
         'Only explicitly approved script hashes are executable. List approvals using read_skill_calculation.',
         {'skill': {'type': 'string'}, 'path': {'type': 'string'},
          'items': {'type': 'array', 'minItems': 1, 'maxItems': 100, 'items': {'type': 'object'}}},
         ['skill', 'path', 'items'], calculate, EffectClass.COMPUTE),
        ('read_skill_calculation', 'Результаты вычисления',
         'Without job_id list approved Python skill scripts and entrypoints. With job_id read saved rows; '
         'continue using next_offset until null.',
         {'job_id': {'type': 'string'}, 'offset': {'type': 'integer', 'minimum': 0}}, [],
         lambda **args: read_job(**args) if args.get('job_id') else {'scripts': approved_scripts()}, EffectClass.READ),
    ]
    for name, title, description, properties, required, action, effect in definitions:
        async def handler(arguments, action=action, name=name):
            from proxy.services.tool_harness_service import _result
            result = await asyncio.to_thread(action, **arguments)
            return _result(tool=name, operation='skill_compute', inputs=[],
                           status='error' if result.get('interrupted') else 'ok',
                           result=result, trace='approved_skill_compute')
        registry.register(ToolRegistration(contract=ToolContract(
            name=name, version='1.0.0', title=title, category='skills', summary=description,
            input_schema={'type': 'object', 'properties': properties, 'required': required,
                          'additionalProperties': False}, result_schema='les_tool_result_v1',
            effect=effect, scopes=('skills',), timeout_seconds=25,
            retry=RetryPolicy.NEVER, idempotency=IdempotencyPolicy.NONE,
            result_budget=ResultBudget(max_chars=32000, max_items=16), model_owned_fields=(),
            provenance='Operator-approved pure calculation; inputs and outputs saved', tags=('skills',)), handler=handler))
