"""Discover selected extensions on demand, within one model tool definition."""
import hashlib


def register_extension_access(harness, names):
    from proxy.services.tool_contract_service import (
        ToolContract, EffectClass, ResultBudget, RetryPolicy, IdempotencyPolicy)
    from proxy.services.tool_registry_service import ToolRegistration
    from proxy.services.tool_harness_service import _result

    names = tuple(sorted(set(names)))
    name = 'use_extensions_' + hashlib.sha256('\n'.join(names).encode()).hexdigest()[:12]
    if harness._registry.get(name):
        return name

    async def handler(arguments):
        operation = arguments['operation']
        target = arguments.get('name', '')
        if operation == 'list':
            offset = arguments.get('offset', 0)
            items = [harness._registry.require(item).contract.public_payload() for item in names]
            page = items[offset:offset + 10]
            result = {'tools': [{key: item[key] for key in ('name', 'title')} |
                                {'summary': item['summary'][:300]} for item in page],
                      'next_offset': offset + len(page) if offset + len(page) < len(items) else None}
        else:
            if target not in names:
                raise ValueError('Этот инструмент не подключён к текущему профилю')
            if operation == 'describe':
                result = harness._registry.require(target).contract.public_payload()
            else:
                result = await harness.call_async(target, arguments.get('arguments', {}))
        failed = operation == 'call' and result.get('status') not in ('ok', 'success')
        return _result(tool=name, operation=operation, inputs=[], status='error' if failed else 'ok',
                       result=result, trace='selected_extension_access')

    contract = ToolContract(
        name=name, version='1.0.0', title='Подключённые навыки и MCP', category='extensions',
        summary='Access user-selected MCP and installed skill tools. List tools, describe a chosen '
                'name to obtain its exact input schema, then call it with arguments. '
                'List pagination uses next_offset. Only approved skill calculations can execute Python; '
                'arbitrary code and commands are unavailable.',
        input_schema={'type': 'object', 'properties': {
            'operation': {'type': 'string', 'enum': ['list', 'describe', 'call']},
            'name': {'type': 'string'}, 'arguments': {'type': 'object'},
            'offset': {'type': 'integer', 'minimum': 0}},
            'required': ['operation'], 'additionalProperties': False},
        result_schema='les_tool_result_v1', effect=EffectClass.COMPUTE, scopes=('extensions',),
        timeout_seconds=35, retry=RetryPolicy.NEVER, idempotency=IdempotencyPolicy.NONE,
        result_budget=ResultBudget(max_chars=40000, max_items=32), model_owned_fields=(),
        provenance='Frozen selected profile extensions', tags=('extensions',))
    harness._registry.register(ToolRegistration(contract=contract, handler=handler))
    return name
