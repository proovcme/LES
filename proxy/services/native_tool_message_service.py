"""Convert canonical tool turns to native chat JSON without changing caller history."""
from collections.abc import Mapping
import json


def native_tool_message(message):
    result = dict(message)
    if result.get('tool_calls') is not None:
        calls = []
        for call in result['tool_calls']:
            function = dict(call['function'])
            arguments = function.get('arguments', {})
            if isinstance(arguments, str):
                try:
                    arguments = json.loads(arguments)
                except ValueError as error:
                    raise ValueError('INVALID_NATIVE_TOOL_ARGUMENTS') from error
            if not isinstance(arguments, Mapping):
                raise ValueError('INVALID_NATIVE_TOOL_ARGUMENTS')
            calls.append({**call, 'function': {**function, 'arguments': dict(arguments)}})
        result['tool_calls'] = calls
    if result.get('role') == 'tool' and result.get('name'):
        result['tool_name'] = result['name']
    return result
