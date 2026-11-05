from types import SimpleNamespace

import pytest

from backend.interface import Chunk, EmbeddingContractError
from proxy.services.lexical_evidence_service import canonicalize_lexical_hits
from proxy.services.chat_section_context_service import ChatSectionReader
from tests.test_light_chat_sections import backend, point


def lexical(p):
    return Chunk(p.payload['text'], '', p.payload['file_name'], .5,
                 {'point_id': str(p.id), 'dataset_id': p.payload['dataset_id']})


@pytest.mark.asyncio
async def test_fts_candidate_recovers_page_and_can_pass_real_parent_reader(tmp_path):
    p = point('Правило и условия его применения.', 4)
    b = await backend(tmp_path, [p])
    try:
        hits = await canonicalize_lexical_hits([lexical(p)], b, ['ds'])
        assert hits[0].meta['qdrant_point_id'] == str(p.id)
        assert hits[0].meta['page'] == 5 and hits[0].meta['source_ref'] == p.payload['source_ref']
        expanded = await ChatSectionReader(b, ['ds']).expand(hits)
        assert expanded.chunks[0].content == p.payload['text']
    finally:
        await b.aclient.close()


@pytest.mark.asyncio
@pytest.mark.parametrize('forbidden', ['navigation', 'inadmissible'])
async def test_fts_never_turns_navigation_into_answer_evidence(tmp_path, forbidden):
    p = point('Навигационная сводка; не дословный источник.')
    p.payload['node_role' if forbidden == 'navigation' else 'evidence_admissible'] = 'navigation' if forbidden == 'navigation' else False
    b = await backend(tmp_path, [p])
    try:
        assert await canonicalize_lexical_hits([lexical(p)], b, ['ds']) == []
    finally:
        await b.aclient.close()


@pytest.mark.asyncio
@pytest.mark.parametrize('mismatch', ['text', 'dataset', 'file', 'scope', 'filter', 'deleted'])
async def test_stale_or_out_of_scope_fts_hits_fail_closed(tmp_path, mismatch):
    p = point('Исходный текст')
    b = await backend(tmp_path, [p])
    hit = lexical(p)
    if mismatch == 'text': hit.content = 'Другой текст'
    if mismatch == 'dataset': hit.meta['dataset_id'] = 'alien'
    if mismatch == 'file': hit.doc_name = 'Другой.pdf'
    if mismatch == 'deleted': await b.aclient.delete('docs', points_selector=[p.id])
    try:
        with pytest.raises(EmbeddingContractError, match='INDEX_CHANGED_DURING_SEARCH'):
            await canonicalize_lexical_hits([hit], b, ['alien'] if mismatch == 'scope' else ['ds'],
                                            ['Другой.pdf'] if mismatch == 'filter' else None)
    finally:
        await b.aclient.close()
