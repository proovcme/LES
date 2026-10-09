import json
import pytest

from backend.inference.bm25_sparse import tokenize, _term_id
from backend.inference.lexical_tokens import tokenize_current
from backend.sparse_index import ensure_current, encode_query
from proxy.services.lexical_index_service import LexicalIndex
from test_light_bm25_index import index, put, scores


@pytest.mark.parametrize('label', ['PE', 'N', 'ОВ', 'ВК', '63', '7', 'BB_63', 'ЩР/2', 'L1', 'СП'])
def test_short_markers_remain_searchable_in_prose(index, label):
    client, journal = index
    put(client, [f'Подключение {label} показано на чертеже.', 'Подключение показано на чертеже.'])
    ensure_current(client, journal)
    assert list(scores(client, journal, label.lower())) == [0]


def test_normalization_does_not_merge_lookalike_scripts_or_double_frequency():
    assert tokenize_current('PE pe\u00a0ＰＥ') == ['pe', 'pe', 'pe']
    assert tokenize_current('N РЕ') == ['n', 'ре']
    assert tokenize_current('BB_63 ЩР‑2 СП 256.1325800.2016') == ['bb_63', 'щр-2', 'сп', '256.1325800.2016']
    assert tokenize_current('в и на для') == []
    assert tokenize('Проводник PE') == ['проводник']  # rollback vocabulary stays frozen


def test_v1_migrates_once_without_changing_payload_or_dense(index):
    client, journal = index
    put(client, ['Проводник PE подключён', 'Другой документ'])
    before = client.retrieve('docs', ids=[0, 1], with_vectors=True)
    # A genuine v1 BM25 index: materialize its old terms and matching contract.
    import math
    from qdrant_client import models
    from backend.inference.bm25_weighted import BM25Profile
    profile = BM25Profile.from_documents([p.payload['text'] for p in before])
    client.update_collection('docs', sparse_vectors_config={
        'bm25_sparse': models.SparseVectorParams(modifier=models.Modifier.NONE)})
    for p in before:
        terms = {k: v * math.log(2) for k, v in profile.document(p.payload['text']).items()}
        client.update_vectors('docs', points=[models.PointVectors(id=p.id, vector={
            'bm25_sparse': models.SparseVector(indices=list(terms), values=list(terms.values()))})])
    from dataclasses import asdict
    journal._save({'phase':'idle', 'revision':'old'})
    old = dict(schema='les.sparse-index.v1', mode='bm25', tokenizer='les.lexical.v1',
               idf='exact-corpus', vector_name='bm25_sparse', physical_collection='docs',
               points=2, profile=asdict(profile), revision='old')
    (journal.directory/'sparse.json').write_text(json.dumps(old))
    assert scores(client, journal, 'PE') == {}
    current = ensure_current(client, journal)
    assert current['tokenizer'] == 'les.lexical.v2'
    assert list(scores(client, journal, 'pe')) == [0]
    assert ensure_current(client, journal) == current
    for old_point, new_point in zip(before, client.retrieve('docs', ids=[0,1], with_vectors=True)):
        assert old_point.payload == new_point.payload
        assert old_point.vector['dense'] == new_point.vector['dense']
    ensure_current(client, journal, mode='tf')
    assert scores(client, journal, 'PE') == {}
    assert encode_query(journal, 'кабель кабель')[_term_id('кабел')] == 2


@pytest.mark.parametrize('label', ['PE', 'N', 'ОВ', '63', 'BB_63', 'ЩР/2'])
def test_fts_short_queries_find_existing_raw_text_and_respect_scope(tmp_path, label):
    db = LexicalIndex(str(tmp_path/'lexical.sqlite'))
    db.upsert_chunks('docs', [dict(point_id=str(i), dataset_id=scope,
        doc_name='Источник.md', text=f'Подключение {label} показано на чертеже.')
        for i, scope in enumerate(['chosen', 'other'])])
    hits = db.search(label.lower(), collection='docs', dataset_ids=['chosen'])
    assert [hit.meta['point_id'] for hit in hits] == ['0']


def test_failure_notice_preserves_reading_position_until_user_requests_focus(monkeypatch):
    from nicegui import Client, ui
    from nicegui.page import page
    from sovushka.components.chat_failure_notice import ChatFailureNotice
    scripts = []
    monkeypatch.setattr(ui, 'run_javascript', scripts.append)
    with Client(page('/failure-notice')):
        notice = ChatFailureNotice()
        bubble = ui.column()
        assert not notice.row.visible
        notice.show(bubble)
        assert notice.row.visible and not scripts
        assert bubble._props['tabindex'] == '-1'
        notice.reveal()
        assert f'c{bubble.id}' in scripts[0] and 'preventScroll' in scripts[0]
        notice.clear()
        assert not notice.row.visible and notice.bubble is None
        notice.reveal()
        assert len(scripts) == 1
