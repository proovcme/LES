"""Collection startup under the same writer lease as ingestion and recovery."""
from qdrant_client.http.exceptions import UnexpectedResponse

from backend import qdrant_support as support
from backend.index_replacement import ReplacementJournal


def _indexed_catalog_count(adapter):
    database = getattr(adapter, 'db', None)
    if database is None:
        return None  # Standalone collection clients have no document catalog.
    try:
        with database._get_conn() as conn:
            return int(conn.execute("SELECT COALESCE(SUM(chunk_count), 0) FROM documents WHERE status='INDEXED' AND chunk_count>0").fetchone()[0])
    except Exception as error:
        raise support.EmbeddingContractError('INDEX_CATALOG_VERIFICATION_UNAVAILABLE') from error


async def _exact_count(adapter, **kwargs):
    try:
        result = await adapter.aclient.count(collection_name=adapter.collection_name, exact=True, **kwargs)
        count = result.count
        if type(count) is not int or count < 0:
            raise ValueError('Invalid exact count')
        return count
    except Exception as error:
        raise support.EmbeddingContractError('INDEX_POINT_COUNT_UNAVAILABLE') from error


async def _create(adapter, named):
    vectors = support.models.VectorParams(size=adapter.vector_size, distance=support.models.Distance.COSINE)
    kwargs = dict(collection_name=adapter.collection_name, vectors_config=vectors)
    if named:
        kwargs.update(vectors_config={support._dense_vector_name(): vectors}, sparse_vectors_config={
            support._sparse_vector_name(): support.models.SparseVectorParams(modifier=support.models.Modifier.IDF)})
    await adapter.aclient.create_collection(**kwargs)


async def ensure_layout(adapter):
    """Never use CollectionInfo.points_count as authority to delete or adopt.

    That field is advisory and may be zero/unknown while committed points exist.
    A failed read is not a 404; an unavailable exact count never means empty.
    """
    journal = ReplacementJournal.for_adapter(adapter)
    with journal.lease():
        try:
            info = await adapter.aclient.get_collection(adapter.collection_name)
        except UnexpectedResponse as error:
            if error.status_code != 404:
                raise support.EmbeddingContractError('INDEX_SERVICE_UNAVAILABLE') from error
            info = None
        except Exception as error:
            raise support.EmbeddingContractError('INDEX_SERVICE_UNAVAILABLE') from error

        named = support._qdrant_schema_mode() == 'named'
        points_count = await _exact_count(adapter) if info is not None else 0
        if journal.pending() is None:
            expected = _indexed_catalog_count(adapter)
            if expected is not None and points_count < expected:
                # Tripwire, not an automatic repair: orphan/staged points may
                # inflate a total, so a successful total is no per-file audit.
                raise support.EmbeddingContractError('INDEX_CATALOG_MISMATCH')
        if info is not None and named:
            compatible, _summary_count = support._named_collection_layout(info, vector_size=adapter.vector_size)
            if not compatible:
                if points_count:
                    raise support.EmbeddingContractError(
                        f'INDEX_SCHEMA_MIGRATION_REQUIRED: collection {adapter.collection_name} has '
                        f'{points_count} points in an incompatible vector schema; explicit migration is required')
                journal.assert_clean()
                support.logger.warning('[INIT] Replacing exactly verified empty legacy collection %s', adapter.collection_name)
                await adapter.aclient.delete_collection(adapter.collection_name)
                info = None
            elif support.index_contract_status().get('status') == 'missing':
                matching = 0
                if points_count:
                    matching = await _exact_count(adapter, count_filter=support.models.Filter(must=[
                        support.models.FieldCondition(key='embedding_fingerprint', match=support.models.MatchValue(
                            value=support.point_embedding_fingerprint()))]))
                if support._can_adopt_missing_contract(points_count=points_count, matching_fingerprint_count=matching):
                    support.write_index_contract(replace=False)

        created = info is None
        if created:
            journal.assert_clean()
            await _create(adapter, named)
        if created or points_count == 0:
            try:
                support.write_index_contract(replace=False)
            except FileExistsError:
                pass  # Existing embedding identity must be validated, never overwritten.
