"""Frozen configuration/HTTP only. Rust owns the complete batch and every processing callback."""
import math
import os
from pathlib import Path

from backend import native_core, native_index_store, qdrant_support as support
from backend.extraction_errors import DocumentExtractionError


def run(adapter, dataset_id, limit, directory, *, sources=None, intake_id=None):
    if not native_core.supports('document_nodes'):
        raise DocumentExtractionError('INDEX_NODE_PREPARATION_UNAVAILABLE')
    from backend.embedding_client import EmbedClient

    chunking = support.chunking_config()
    tokenizer = getattr(chunking.get('len_fn'), 'asset', None)
    if chunking['unit'] == 'tokens' and not isinstance(tokenizer, dict):
        raise DocumentExtractionError('DOCUMENT_TOKENIZER_UNAVAILABLE')
    try:
        window = max(1, int(os.getenv('RAG_PARENT_WINDOW_CHUNKS', '4')))
    except ValueError:
        window = 4
    descriptor = support._embedding_cache_descriptor()
    embedder = getattr(adapter, 'embed_parse', None) or adapter.embed
    if not isinstance(embedder, EmbedClient):
        raise DocumentExtractionError('EMBEDDING_NATIVE_EXECUTION_REQUIRED')
    frozen = embedder.for_index(descriptor)
    connection = getattr(frozen, '_frozen_connection', None)
    vector_names = ({'dense':support._dense_vector_name(), 'sparse':support._sparse_vector_name()}
                    if support._qdrant_schema_mode() == 'named' else None)
    try:
        maximum = max(1, int(os.getenv('RAG_PARSE_MAX_ATTEMPTS', '4')))
    except ValueError:
        maximum = 4
    try:
        seconds = float(os.getenv('RAG_PARSE_FILE_TIMEOUT_SEC', '1800'))
        if not math.isfinite(seconds):seconds = 1800
    except ValueError:
        seconds = 1800
    source_root = adapter.content_dir.resolve()
    content_root = Path(os.getenv('LES_INDEX_CONTENT_ROOT') or source_root).resolve()
    request = {
        'schema':'les.index-batch.v1', 'database':str(Path(adapter.db.db_path).resolve()),
        'source_root':str(source_root), 'content_root':str(content_root), 'spool_directory':str(directory),
        'dataset_id':dataset_id, 'collection':adapter.collection_name, 'limit':limit,
        'allow_unbounded':os.getenv(support.ALLOW_UNBOUNDED_PARSE, '').lower() in ('1','true','yes'),
        'intake_id':intake_id,
        'sources':None if sources is None else [{'file_name':name,'source_path':path} for name,path in sources],
        'embedding':descriptor, 'embedding_fingerprint':support._embedding_cache_fingerprint(descriptor),
        'vector_names':vector_names, 'embed_batch':support.EMBED_BATCH, 'upsert_batch':support.UPSERT_BATCH,
        'verify_every':support.VERIFY_POINTS_EVERY, 'cache_enabled':support.CHUNK_HASH_CACHE,
        'max_attempts':maximum, 'release_workers_after':True,
        'chunking':{name:value for name,value in chunking.items() if name!='len_fn'},
        'node_preparation':{'unit':chunking['unit'], 'size':chunking['chunk_size'],
            'minimum':support.FINAL_MIN_CHUNK, 'tokenizer':tokenizer, 'window':window},
        'format_worker':{'timeout_ms':int(max(.1,min(3600,seconds))*1000),
            'minimum':support.MIN_CHUNK, 'final_minimum':support.FINAL_MIN_CHUNK,
            'table_row_limit':support.TABLE_ROW_INDEX_MAX_CHUNKS,
            'pdf_page_size':support._pdf_page_node_max_chars(),
            'pdf_page_overlap':min(support._pdf_page_node_overlap_chars(),support._pdf_page_node_max_chars()//3)},
    }
    if connection is not None:
        # Same explicit registry selection as canonical embedding transport.
        request['embedding_execution'] = {'revision_id':connection.revision_id,
            'allow_remote':connection.locality.value == 'remote'}
    return native_index_store.execute_batch(request)
