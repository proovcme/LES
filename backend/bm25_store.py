"""Bounded adapter to the mandatory Rust BM25 owner; no SQL fallback."""
from contextlib import contextmanager
from itertools import islice
import json

from backend import native_core
from backend.inference.lexical_tokens import tokenize_current
from backend.interface import EmbeddingContractError
from backend.lexical_preparation import prepare


def _call(action, **arguments):
    try:
        return native_core.required('bm25', request=dict(action=action, **arguments))
    except native_core.NativeCoreError as error:
        if str(error).startswith('INDEX_SPARSE_'):
            raise EmbeddingContractError(str(error)) from error
        raise


def _database(journal):
    return str((journal.directory / 'bm25.sqlite').resolve())


@contextmanager
def writer(journal):
    handle = _call('begin', database=_database(journal))['handle']
    try:
        yield handle
    finally:
        _call('rollback', handle=handle)


def metadata(journal):
    return tuple(_call('metadata', database=_database(journal))['metadata'])


def reset(handle):
    _call('reset', handle=handle)


def clear_scope(handle, scope):
    if 'ids' in scope:
        scope = dict(ids=[json.dumps(value) for value in scope['ids']])
    _call('clear', handle=handle, scope=scope)


def put(handle, points):
    iterator = iter(points)
    while batch := list(islice(iterator, 256)):
        terms = prepare([str((point.payload or {}).get('text') or '') for point in batch])
        rows = [dict(id=json.dumps(point.id), terms=counts, payload={
            key: (point.payload or {}).get(key)
            for key in ('dataset_id', 'file_name', 'node_role', 'ancestor_ids')})
            for point, counts in zip(batch, terms, strict=True)]
        _call('put', handle=handle, points=rows)


def commit(handle, revision, expected_count):
    return tuple(_call('commit', handle=handle, revision=revision,
                       expected_count=expected_count)['metadata'])


def search(journal, text, *, limit=24, dataset_ids=None, doc_filter=None,
           node_roles=None, ancestor_ids=None):
    stamp = journal.read_stamp()
    terms = sorted(set(tokenize_current(text)))
    if not terms:
        return []
    value = _call('search', database=_database(journal), revision=stamp, terms=terms,
                  limit=int(limit), dataset_ids=dataset_ids, doc_filter=doc_filter,
                  node_roles=node_roles, ancestor_ids=ancestor_ids)
    journal.assert_unchanged(stamp)
    return [(json.loads(point_id), score) for point_id, score in value['hits']]
