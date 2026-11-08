"""Retained caller facade. Rust owns model assets, tokenization, inference and worker lifetime."""
import hashlib
import os
from pathlib import Path
from backend import native_core


def configured() -> bool:
    return bool(os.environ.get('LES_NATIVE_EMBEDDING_SPEC'))


def encode(model_id, texts, dimension=None, *, release_after=False, connection_revision_id=None):
    spec = Path(os.environ.get('LES_NATIVE_EMBEDDING_SPEC', ''))
    program = Path(os.environ.get('LES_NATIVE_EMBEDDING_WORKER', ''))
    if not spec.is_absolute() or not program.is_absolute() or not spec.is_file() or not program.is_file():
        raise RuntimeError('EMBEDDING_NATIVE_WORKER_MISSING')
    content = spec.read_bytes()
    if len(content) > 1024 * 1024:
        raise RuntimeError('EMBEDDING_SPEC_INVALID')
    value = native_core.required('worker_embeddings', request={
        'schema': 'les.worker-host.v1', 'program': str(program),
        'args': ['--serve-embeddings', str(spec)], 'directory': str(spec.parent),
        'timeout_ms': 120000, 'context': {'spec_sha256': hashlib.sha256(content).hexdigest()},
        'input': {'model_id': model_id, 'dimension': dimension, 'texts': list(texts),
                  'connection_revision_id': connection_revision_id},
    })
    if value.get('status') != 'completed':
        raise RuntimeError(value.get('cause') or value.get('code') or 'EMBEDDING_WORKER_FAILED')
    result = value.get('result') or {}
    if result.get('status') != 'ok':
        raise RuntimeError(result.get('code') or 'EMBEDDING_INFERENCE_FAILED')
    vectors = result['result']['vectors']
    if release_after:
        native_core.required('worker_release_idle')
    return vectors
