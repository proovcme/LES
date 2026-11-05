"""Bounded native bridge for document, storage and harness operations."""
from __future__ import annotations

import ctypes
from functools import lru_cache
import hashlib
import json
import os
from pathlib import Path
import sys

MAX_INPUT = 8 * 1024 * 1024
MAX_OUTPUT = 32 * 1024 * 1024


class NativeCoreError(RuntimeError):
    pass


def _invoke(library, data):
    source = ctypes.create_string_buffer(data)
    length = ctypes.c_size_t()
    pointer = library.les_core_call(source, len(data), ctypes.byref(length))
    if not pointer:
        raise NativeCoreError('CORE_CALL_FAILED')
    try:
        if length.value > MAX_OUTPUT:
            raise NativeCoreError('CORE_OUTPUT_LIMIT')
        result = json.loads(ctypes.string_at(pointer, length.value))
    except (ValueError, UnicodeError) as error:
        raise NativeCoreError('CORE_INVALID_RESPONSE') from error
    finally:
        library.les_core_free(pointer, length.value)
    if not isinstance(result, dict) or result.get('ok') is not True:
        raise NativeCoreError(str(result.get('code', 'CORE_INVALID_RESPONSE')) if isinstance(result, dict) else 'CORE_INVALID_RESPONSE')
    if not isinstance(result.get('result'), dict):
        raise NativeCoreError('CORE_INVALID_RESPONSE')
    return result['result']


def library_path():
    configured = os.getenv('LES_CORE_LIBRARY', '').strip()
    if configured:
        return Path(configured).resolve()
    name = 'les_core.dll' if sys.platform == 'win32' else ('libles_core.dylib' if sys.platform == 'darwin' else 'libles_core.so')
    return Path(__file__).resolve().parents[2] / 'native/core' / name


@lru_cache(maxsize=2)
def _load(path: Path):
    try:
        receipt = json.loads((path.parent / 'core-receipt.json').read_text(encoding='utf-8'))
        with path.open('rb') as stream:
            checksum = hashlib.file_digest(stream, 'sha256').hexdigest()
        if (receipt.get('schema') != 'les.core-asset.v1' or receipt.get('abi') != 1 or
                receipt.get('file') != path.name or receipt.get('sha256') != checksum):
            raise NativeCoreError('CORE_ASSET_MISMATCH')
        library = ctypes.CDLL(str(path))
        library.les_core_abi_version.restype = ctypes.c_uint32
        if library.les_core_abi_version() != 1:
            raise NativeCoreError('CORE_ABI_MISMATCH')
        library.les_core_call.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t))
        library.les_core_call.restype = ctypes.c_void_p
        library.les_core_free.argtypes = (ctypes.c_void_p, ctypes.c_size_t)
        library.les_core_free.restype = None
        health = _invoke(library, b'{"schema":"les.core.v1","operation":"health"}')
        if (health.get('abi') != 1 or health.get('tokenizer') != 'les.lexical.v2' or
                not {'prepare_terms','fingerprint','job_patch','job_recovery'} <= set(health.get('operations', []))):
            raise NativeCoreError('CORE_CAPABILITY_MISMATCH')
        return library
    except (OSError, ValueError, AttributeError) as error:
        raise NativeCoreError('CORE_ASSET_UNAVAILABLE') from error


def mode():
    value = os.getenv('LES_CORE_MODE', 'auto').strip().lower()
    if value not in {'auto', 'python', 'native'}:
        raise NativeCoreError('CORE_MODE_INVALID')
    return value


def available():
    selected = mode()
    if selected == 'python':
        return False
    path = library_path()
    if not path.is_file():
        if selected == 'native':
            raise NativeCoreError('CORE_ASSET_UNAVAILABLE')
        return False
    _load(path)  # A present but corrupt/incompatible asset must never load silently.
    return True


def call(operation, **arguments):
    if not available():
        return None
    return required(operation, **arguments)


def supports(operation):
    if not available():
        return False
    health = _invoke(_load(library_path()), b'{"schema":"les.core.v1","operation":"health"}')
    if operation in health.get('operations', []):
        return True
    if mode() == 'native':
        raise NativeCoreError('CORE_CAPABILITY_MISMATCH')
    return False  # Compatibility with an explicitly retained older core asset.


def required(operation, **arguments):
    """Mandatory native owner, independent of the optional accelerator selector."""
    data = json.dumps({'schema': 'les.core.v1', 'operation': operation, **arguments},
                      ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode('utf-8')
    if len(data) > MAX_INPUT:
        raise NativeCoreError('CORE_INPUT_LIMIT')
    return _invoke(_load(library_path()), data)
