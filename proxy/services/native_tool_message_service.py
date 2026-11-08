"""Compatibility facade; canonical-to-native tool turns are constructed by Rust."""
from collections.abc import Mapping
from backend import native_core


def _plain(value):
    if isinstance(value, Mapping):return {key: _plain(item) for key,item in value.items()}
    if isinstance(value, (list,tuple)):return [_plain(item) for item in value]
    return value


def native_tool_message(message):
    try:
        return native_core.required('model_request',request={
            'schema':'les.model-request.v1','action':'native_message','message':_plain(message),
        })['value']
    except native_core.NativeCoreError as error:
        raise ValueError(str(error)) from error
