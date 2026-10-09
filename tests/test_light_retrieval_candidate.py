"""Candidate-only regression tests: ranking, provenance, complete sections, budgets."""
import json
from types import SimpleNamespace

import pytest
from qdrant_client import QdrantClient, models

from backend.inference.bm25_weighted import BM25Profile
from backend.retrieval_candidate import RetrievalCandidate, sparse_vector
from proxy.services.section_evidence_candidate import Fragment, pack_sections


def point(index, text, *, dataset="d", file="document.pdf", parent="section", ordinal=None):
    return models.PointStruct(id=index, vector={"dense": [1., 0.]}, payload={
        "dataset_id": dataset, "file_name": file, "parent_id": parent,
        "chunk_ord": index if ordinal is None else ordinal, "source_page": index + 1,
        "text": text, "node_role": "evidence", "evidence_admissible": True})


def fragment(index, text="Текст", **kwargs):
    return Fragment.from_point(point(index, text, **kwargs))


def test_bm25_saturates_tf_and_normalizes_document_length():
    model = BM25Profile(10)
    assert list(model.document("кабель").values()) == pytest.approx([2.2 / 1.39])
    assert max(model.document("кабель " * 100).values()) < 2.2
    short = next(iter(model.document("кабель").values()))
    long = model.document("кабель " + "посторонний " * 100)
    term = next(iter(model.query("кабель")))
    assert short > long[term]
    assert model.query("кабель кабель") == model.query("кабель")


@pytest.mark.parametrize("average,k1,b", [(0,1.2,.75), (float('nan'),1.2,.75), (1,0,.75), (1,1.2,2)])
def test_invalid_sparse_profile_is_rejected(average, k1, b):
    with pytest.raises(ValueError):
        BM25Profile(average, k1, b)


def test_language_and_notation_are_preserved_without_changing_tokenizer():
    model = BM25Profile(10)
    assert model.query("ёжик") == model.query("ЕЖИК")
    assert model.query("ПвБШп 4х300 0,4кВ СП 256.1325800.2016")
    assert model.query("PE")
    assert model.document("   \n\t") == {}


def test_parent_completion_adds_unretrieved_exception_with_own_citation():
    hit = fragment(1, "Требование действует.")
    exception = fragment(2, "Исключение: для существующих зданий иной порядок.")
    calls = []
    def read(seed):
        calls.append(seed.section_key)
        return [exception, hit]
    packed = pack_sections([hit], read, max_tokens=4000, count_tokens=len)
    section = packed["evidence"]["sections"][0]
    assert section["complete"]
    assert [item["citation_id"] for item in section["fragments"]] == ['1', '2']
    assert packed['source_map']['2']['quote'] == exception.text
    assert packed['source_map']['2']['pages'] == [3]
    assert len(calls) == 1


def test_budget_includes_labels_and_sources_and_marks_partial_parent():
    hit, large = fragment(1, "Короткий ответ."), fragment(2, "Длинное условие " * 1000)
    packed = pack_sections([hit], lambda _: [hit, large], max_tokens=700, count_tokens=len)
    assert len(packed['context']) <= 700
    assert packed['tokens'] == len(packed['context'])
    assert not packed['evidence']['sections'][0]['complete']
    assert set(packed['source_map']) == {'1'}
    assert 'Длинное условие' not in packed['context']


def test_parent_expansion_cannot_evict_another_retrieved_source():
    first = fragment(1, 'Ответ первого документа')
    extra = fragment(2, 'Дополнительный текст ' * 24)
    second = fragment(3, 'Ответ другого документа', file='other.pdf')
    result = pack_sections([first,second], lambda seed: [first,extra] if seed.file_name==first.file_name else [second],
                           max_tokens=1050,count_tokens=len)
    assert {'1','3'} <= set(result['source_map'])
    assert result['omitted_hits'] == 0
    assert len(result['context']) <= 1050


def test_grouping_does_not_cross_sources_or_change_rank_and_reads_once():
    a = fragment(3, parent='shared')
    b = fragment(1, file='other.pdf', parent='shared')
    c = fragment(4, parent='shared')
    calls = []
    def read(seed):
        calls.append(seed.section_key)
        return [a,c] if seed.file_name == a.file_name else [b]
    result = pack_sections([a,b,c], read, max_tokens=5000, count_tokens=len)
    assert len(calls) == 2
    assert [s['file_name'] for s in result['evidence']['sections']] == ['document.pdf','other.pdf']
    assert len(result['source_map']) == 3


@pytest.mark.parametrize('bad', [fragment(2, dataset='other'), fragment(2, file='other.pdf'), fragment(2, parent='other')])
def test_reader_cannot_leak_another_scope(bad):
    hit = fragment(1)
    with pytest.raises(ValueError, match='boundary'):
        pack_sections([hit], lambda _: [hit,bad], max_tokens=4000, count_tokens=len)


def test_changed_generation_and_inadmissible_text_are_rejected():
    hit = fragment(1)
    with pytest.raises(ValueError, match='Index changed'):
        pack_sections([hit], lambda _: [fragment(1, 'Другой текст')], max_tokens=4000, count_tokens=len)
    bad = point(2, 'Оглавление')
    bad.payload['node_role'] = 'navigation'
    with pytest.raises(ValueError, match='Navigation'):
        Fragment.from_point(bad)


@pytest.mark.parametrize('field', ['page','source_page','page_number'])
def test_existing_parser_page_metadata_survives_in_citation(field):
    p = point(1, 'Доказательство на странице 14')
    p.payload.pop('source_page')
    p.payload[field] = 14
    hit = Fragment.from_point(p)
    result = pack_sections([hit], lambda _: [hit], max_tokens=1500, count_tokens=len)
    assert result['source_map']['1']['pages'] == [14]


def test_real_parent_reader_follows_pagination_without_crossing_file():
    rows = [point(1,'Начало'),point(2,'Конец')]
    offsets = []
    def scroll(collection, **kwargs):
        offsets.append(kwargs['offset'])
        filters = {condition.key:condition.match.value for condition in kwargs['scroll_filter'].must}
        assert filters == {'dataset_id':'d','file_name':'document.pdf','parent_id':'section'}
        return ([rows[0]],'next') if kwargs['offset'] is None else ([rows[1]],None)
    candidate = RetrievalCandidate(SimpleNamespace(scroll=scroll),'les_trial_pages',BM25Profile(10))
    assert len(candidate.read_section(Fragment.from_point(rows[0]))) == 2
    assert offsets == [None,'next']


def test_real_qdrant_queries_and_parent_reading_are_scoped_and_rerank_is_optional():
    points = [point(1,'кабель медный'), point(2,'условие исключение'),
              point(3,'кабель кабель',dataset='other')]
    profile = BM25Profile.from_documents([p.payload['text'] for p in points])
    with_client = QdrantClient(':memory:')
    try:
        with_client.create_collection('les_trial_test',
            vectors_config={'dense':models.VectorParams(size=2,distance=models.Distance.COSINE)},
            sparse_vectors_config={name:models.SparseVectorParams(modifier=models.Modifier.IDF)
                                   for name in ('bm25_sparse','sparse_tf')})
        for p in points:
            p.vector['bm25_sparse'] = sparse_vector(profile.document(p.payload['text']))
            p.vector['sparse_tf'] = sparse_vector(profile.query(p.payload['text']))
        with_client.upsert('les_trial_test',points=points,wait=True)
        candidate = RetrievalCandidate(with_client, 'les_trial_test', profile)
        hits = candidate.search('кабель',[1.,0.],['d'],limit=2)
        assert {h.point_id for h in hits} == {'1','2'}
        assert {h.point_id for h in candidate.read_section(hits[0])} == {'1','2'}
        assert candidate.search('кабель',[1.,0.],['d'],limit=2,
            rerank=lambda q,items:[i.point_id for i in reversed(items)]) == list(reversed(hits))
        with pytest.raises(ValueError, match='permutation'):
            candidate.search('кабель',[1.,0.],['d'],rerank=lambda q,items:['fake'])
        with pytest.raises(ValueError, match='datasets'):
            candidate.search('кабель',[1.,0.],[])
        with pytest.raises(ValueError, match='isolated'):
            RetrievalCandidate(with_client,'les_rag',profile)
    finally:
        with_client.close()
