"""FTS proposes point IDs; current Qdrant payloads own evidence/provenance."""
from backend.interface import Chunk, EmbeddingContractError


async def canonicalize_lexical_hits(chunks, backend, dataset_ids, doc_filter=None):
    pending = [chunk for chunk in chunks if (getattr(chunk, 'meta', None) or {}).get('point_id')
               and not (getattr(chunk, 'meta', None) or {}).get('qdrant_point_id')]
    if not pending:
        return chunks
    if not hasattr(backend, 'aclient'):
        raise EmbeddingContractError('INDEX_LEXICAL_VERIFICATION_UNAVAILABLE')
    ids = list(dict.fromkeys(str(chunk.meta['point_id']) for chunk in pending))
    records = await backend.aclient.retrieve(collection_name=backend.collection_name, ids=ids,
                                             with_payload=True, with_vectors=False)
    payloads = {str(point.id): point.payload or {} for point in records}
    result = []
    for chunk in chunks:
        meta = getattr(chunk, 'meta', None) or {}
        if not meta.get('point_id') or meta.get('qdrant_point_id'):
            result.append(chunk)
            continue
        point_id = str(meta['point_id'])
        payload = payloads.get(point_id)
        if not payload or payload.get('text') != chunk.content or payload.get('file_name') != chunk.doc_name or (
            str(payload.get('dataset_id') or '') != str(meta.get('dataset_id') or '') or
            dataset_ids is not None and payload.get('dataset_id') not in dataset_ids or
            doc_filter and payload.get('file_name') not in doc_filter
        ):
            raise EmbeddingContractError('INDEX_CHANGED_DURING_SEARCH')
        if payload.get('node_role') == 'navigation' or payload.get('evidence_admissible') is False:
            continue
        result.append(Chunk(chunk.content, str(payload.get('doc_id') or ''), chunk.doc_name, chunk.score,
                            {**meta, **payload, 'qdrant_point_id': point_id}))
    return result
