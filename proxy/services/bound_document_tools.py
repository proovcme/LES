"""Bound document tools share the mandatory context budget without replaying logs."""


def document_tool_context(*tools):
    result = {}
    for tool in tools:
        if tool is not None:
            for key, objects in tool.model_context().items():
                result.setdefault(key, []).extend(objects)
    return result


def bound_contract_shortlist(shortlist, contracts, limit):
    bound = [contract for contract in contracts if contract is not None]
    names = {contract['name'] for contract in bound}
    if len(bound) > limit:
        raise ValueError('Лимит инструментов меньше числа явно привязанных инструментов документов')
    shortlist['tools'] = [*bound, *[item for item in shortlist.get('tools', []) if item['name'] not in names]][:limit]
    return {item['name'] for item in shortlist['tools']}
