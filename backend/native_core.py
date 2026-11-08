"""Bounded native bridge for document, storage and harness operations."""
from __future__ import annotations

import ctypes
from collections.abc import Mapping
from functools import lru_cache
import hashlib
import json
import logging
import os
from pathlib import Path
import sys

MAX_INPUT = 8 * 1024 * 1024
MAX_OUTPUT = 32 * 1024 * 1024


class NativeCoreError(RuntimeError):
    pass


def _invoke(library, data, *, entry=None, args=()):
    source = ctypes.create_string_buffer(data)
    length = ctypes.c_size_t()
    invoke = library.les_core_call if entry is None else entry
    pointer = invoke(source, len(data), ctypes.byref(length), *args)
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
    data = _request(operation, **arguments)
    return _invoke(_load(library_path()), data)


def _request(operation, **arguments):
    data = json.dumps({'schema': 'les.core.v1', 'operation': operation, **arguments},
                      ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode('utf-8')
    if len(data) > MAX_INPUT:
        raise NativeCoreError('CORE_INPUT_LIMIT')
    return data


_TOKEN_COUNT = ctypes.CFUNCTYPE(ctypes.c_size_t, ctypes.c_void_p, ctypes.c_size_t)
_INDEX_FILE = ctypes.CFUNCTYPE(ctypes.c_uint32, ctypes.c_size_t, ctypes.c_uint64)
_INDEX_BRIDGE = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_uint8, ctypes.c_void_p,
                                ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t))


class LocalTokenizer:
    def __init__(self, path):
        self.asset = {'path': str(Path(path).resolve())}
        info = required('document', request={'action': 'tokenizer_info', 'tokenizer': self.asset})
        self.asset['sha256'] = info['sha256']

    def __call__(self, text):
        return required('document', request={'action': 'token_count', 'tokenizer': self.asset, 'text': text})['count']


class ChatStream:
    def __init__(self):
        self.library = _load(library_path())
        create = self.library.les_core_chat_stream_new
        create.argtypes, create.restype = (), ctypes.c_void_p
        self.push = self.library.les_core_chat_stream_push
        self.push.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t), ctypes.c_void_p)
        self.push.restype = ctypes.c_void_p
        self.free = self.library.les_core_chat_stream_free
        self.free.argtypes, self.free.restype = (ctypes.c_void_p,), None
        self.handle = create()

    def submit(self, event):
        data = json.dumps(event, ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode('utf-8')
        if len(data) > MAX_INPUT:
            raise NativeCoreError('UPSTREAM_RESPONSE_TOO_LARGE')
        return _invoke(self.library, data, entry=self.push, args=(self.handle,))

    def close(self):
        if self.handle:
            self.free(self.handle)
            self.handle = None


def dispatch_index_batch(database, intake_id, sources, process, *, dataset_id=None):
    library = _load(library_path())
    entry = library.les_core_index_batch
    entry.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t), _INDEX_FILE)
    entry.restype = ctypes.c_void_p
    errors = []

    @_INDEX_FILE
    def visit(index, attempts):
        try:
            return process(index, attempts)
        except BaseException as error:
            errors.append(error)
            return 2

    request = {'database': str(Path(database).resolve()), 'intake_id': intake_id, 'dataset_id': dataset_id,
               'sources': [{'file_name': name, 'source_path': path} for name, path in sources]}
    data = json.dumps(request, ensure_ascii=False).encode('utf-8')
    if len(data) > MAX_INPUT:
        raise NativeCoreError('CORE_INPUT_LIMIT')
    try:
        return _invoke(library, data, entry=entry, args=(visit,))
    except NativeCoreError:
        if errors:
            raise errors[0]
        raise


def _run_json_bridge(entry_name, request, bridge):
    """One synchronous, borrowed JSON callback; no callback survives the native owner."""
    library = _load(library_path())
    entry = getattr(library, entry_name)
    entry.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t), _INDEX_BRIDGE)
    entry.restype = ctypes.c_void_p
    response_buffer = None

    def wire_object(value):
        # Model transports freeze tool calls with MappingProxyType. Preserve
        # their JSON structure, including nested mappings, without stringifying
        # opaque objects or discarding native call IDs/arguments.
        if isinstance(value, Mapping):
            return dict(value)
        raise TypeError(f'Unsupported native wire object: {type(value).__name__}')

    @_INDEX_BRIDGE
    def exchange(operation, pointer, length, result_size):
        nonlocal response_buffer
        result_size[0] = 0
        try:
            response = bridge(operation, json.loads(ctypes.string_at(pointer, length)))
            encoded = json.dumps(response, ensure_ascii=False, separators=(',', ':'),
                                 allow_nan=False, default=wire_object).encode('utf-8')
            if len(encoded) > MAX_OUTPUT:
                raise NativeCoreError('CORE_OUTPUT_LIMIT')
        except BaseException as error:
            code = str(getattr(error, 'code', '') or '')
            if not code and isinstance(error, NativeCoreError):
                code = str(error)
            logging.getLogger(__name__).warning('Native bridge entry=%s code=%s error_type=%s',
                entry_name, code or 'CORE_BRIDGE_FAILED', type(error).__name__)
            encoded = json.dumps({'error': {'code': code, 'error_type': type(error).__name__,
                'message': str(error)[:2000].encode('utf-8', errors='replace').decode('utf-8')}},
                ensure_ascii=True).encode('ascii')
        response_buffer = ctypes.create_string_buffer(encoded)
        result_size[0] = len(encoded)
        return ctypes.addressof(response_buffer)

    encoded = json.dumps(request, ensure_ascii=False, allow_nan=False).encode('utf-8')
    if len(encoded) > MAX_INPUT:
        raise NativeCoreError('CORE_INPUT_LIMIT')
    return _invoke(library, encoded, entry=entry, args=(exchange,))


def run_index_batch(request, bridge):
    """Selection, attempts, state and publication outcomes are Rust-owned."""
    return _run_json_bridge('les_core_index_run', request, bridge)


def run_research(request, bridge):
    """The native owner drives model/tool iterations; this is only an IO codec."""
    return _run_json_bridge('les_core_research_run', request, bridge)


def run_json_document(request, bridge):
    """Only bounded text IO; Rust owns parsing, limits and rendering."""
    return _run_json_bridge('les_core_json_run', request, bridge)


def run_source_nodes(request, bridge):
    """Rust builds source nodes; the callback transports text/passports/results."""
    return _run_json_bridge('les_core_source_nodes_run', request, bridge)


def run_workbook(request, bridge):
    """Run the built-in calculator with an immutable streamed snapshot."""
    return _run_json_bridge('les_core_workbook_run', request, bridge)


def prepare_index_points(request, nodes, embed):
    """Let the core walk nodes and request only the vector batches it needs."""
    library = _load(library_path())
    entry = library.les_core_index_points
    entry.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t), _INDEX_BRIDGE)
    entry.restype = ctypes.c_void_p
    response_buffer = None
    callback_error = None

    def serialize(value):
        return json.dumps(value, ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode('utf-8')

    @_INDEX_BRIDGE
    def exchange(operation, pointer, length, result_size):
        nonlocal response_buffer, callback_error
        try:
            payload = json.loads(ctypes.string_at(pointer, length))
            if operation == 0:
                offset, limit = payload["offset"], payload["limit"]
                batch = nodes[offset:offset + limit]
                encoded = serialize(batch)
                while len(encoded) > MAX_OUTPUT and len(batch) > 1:
                    batch = batch[:len(batch) // 2]
                    encoded = serialize(batch)
            else:
                vectors = embed(payload)
                try:
                    encoded = serialize(vectors)
                except (ValueError, TypeError) as error:
                    raise NativeCoreError("EMBEDDING_VECTOR_INVALID") from error
            if len(encoded) > MAX_OUTPUT:
                raise NativeCoreError("INDEX_POINT_BATCH_LIMIT")
            response_buffer = ctypes.create_string_buffer(encoded)
            result_size[0] = len(encoded)
            return ctypes.addressof(response_buffer)
        except BaseException as error:
            callback_error = error
            result_size[0] = 0
            return None

    try:
        return _invoke(library, serialize(request), entry=entry, args=(exchange,))
    except NativeCoreError:
        if callback_error is not None:
            raise callback_error
        raise


def split_chunks(text, size, overlap, *, kind='structured', tokenizer=None):
    if not supports('document_chunks'):
        return None
    request = dict(action='chunk_text', text=text, size=size, overlap=overlap, kind=kind)
    return _measure_chunks(request, tokenizer)['chunks']


def embedding_chunk_finalizer(size, minimum, tokenizer):
    if not supports('embedding_chunks'):
        return None
    library = _load(library_path())

    def finalize(text):
        request = dict(action='embedding_chunks', text=text, size=size, minimum=minimum)
        return _measure_chunks(request, tokenizer, library=library)

    return finalize


def _measure_chunks(request, tokenizer, *, library=None):
    if library is None:
        library = _load(library_path())
    if isinstance(tokenizer, LocalTokenizer):
        request['tokenizer'] = tokenizer.asset
        return _invoke(library, _request('document', request=request))
    if tokenizer is None:
        return _invoke(library, _request('document', request=request))
    data = json.dumps(request, ensure_ascii=False, allow_nan=False).encode('utf-8')
    if len(data) > MAX_INPUT:
        raise NativeCoreError('CORE_INPUT_LIMIT')
    # ctypes releases the GIL for the native split and reacquires it only for
    # individual lengths. The bound tokenizer remains local to this call.
    @_TOKEN_COUNT
    def measure(pointer, length):
        try:
            return tokenizer(ctypes.string_at(pointer, length).decode('utf-8'))
        except Exception:
            return ctypes.c_size_t(-1).value

    entry = library.les_core_split_chunks
    entry.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t), _TOKEN_COUNT)
    entry.restype = ctypes.c_void_p
    return _invoke(library, data, entry=entry, args=(measure,))
