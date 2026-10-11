"""Resource deferral, exact table evidence and multilingual source fidelity."""
from io import BytesIO
import json
from pathlib import Path
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from backend.document_catalog import MetaDB
from backend.interface import Chunk
from backend.qdrant_ingestion import QdrantIngestion
from backend.qdrant_nodes import QdrantNodes
from backend import qdrant_support as support
from proxy.routers.dataset_uploads import _record_background_parse_error
from proxy.services.chat_prompt_support import clean_visible_text, source_excerpts
from proxy.services.kot_service import analyze_question
from proxy.services.lexical_index_service import RetrievalTrace
from proxy.services.retrieval_quality_service import evaluate_retrieval_quality
from proxy.services.saferag_service import build_context, source_map_for_context
from tools.evaluate_light_chat import coverage, materialize, stream_final


def test_multilingual_quotes_survive_presentation():
    text = 'Сектор 日本語資料, 中文, 한국어; код JP-27.'
    assert clean_visible_text(text) == text
    assert source_excerpts([Chunk(text, 'd', 'catalog.md', .9, {})])[0]['text'] == text


def test_ranking_score_is_diagnostic_not_model_evidence():
    chunks = [Chunk('Verified original text.', 'd', 'a.md', .999, {'page': 2})]
    context = build_context(chunks, max_chars=500, include_metadata=True)
    assert 'score=' not in context and 'стр. 2' in context
    assert source_map_for_context(chunks, max_chars=500)[0]['score'] == .999


@pytest.mark.parametrize('q,text,covered', [
    ('Срок доставки заказа?', 'Заказ доставляют за семь дней.', True),
    ('Код U-15?', 'Код U-150 имеет другую запись.', False),
])
def test_coverage_uses_bm25_token_boundaries(q,text,covered):
    result = evaluate_retrieval_quality(question=q,
        chunks=[Chunk(text,'d','note.md',.8,{})],
        trace=RetrievalTrace(mode='hybrid',score_kind='rrf'),kot=analyze_question(q))
    if covered: assert result.term_coverage > 0
    else:
        # Generic word "код" is covered; the specific identifier is not.
        assert result.term_coverage < 1


@pytest.mark.asyncio
async def test_resource_gate_preserves_pending_counts_and_does_not_consume_attempt(tmp_path):
    db = MetaDB(str(tmp_path/'meta.db')); dataset=db.create_dataset('Synthetic')
    with db._get_conn() as c:
        c.execute("INSERT INTO documents(id,dataset_id,file_name,status,chunk_count,parse_attempts) VALUES('d',?,'table.csv','PENDING',3,0)",(dataset,))
    class Backend:
        async def mark_document_deferred(self, dataset_id, document_id, reason):
            db.mark_document_deferred(dataset_id,document_id,reason)
        async def mark_document_error(self,*args): pytest.fail('Resource gate is not a document failure')
    await _record_background_parse_error(SimpleNamespace(backend=Backend()),dataset_id=dataset,
        document_id='d',error=HTTPException(429,'Awaiting free memory'))
    with db._get_conn() as c:
        row=dict(c.execute("SELECT * FROM documents WHERE id='d'").fetchone())
    assert row['status']=='PENDING' and row['stage']=='WAITING_RESOURCES'
    assert row['chunk_count']==3 and row['parse_attempts']==0 and row['retryable']==1
    assert db.get_pending_files(dataset)==['table.csv']


@pytest.mark.parametrize('kind',['csv','xlsx'])
def test_unknown_table_headers_keep_original_rows(tmp_path,kind):
    name,data=materialize({'id':'inventory','format':kind,'rows':[['Артикул','Остаток'],['U-15',23],['U-51',32]]})
    path=tmp_path/name;path.write_bytes(data)
    class Adapter(QdrantIngestion,QdrantNodes): pass
    nodes=Adapter()._convert_file(path,tmp_path,name,'ds',support.MarkdownNodeParser(),
        support.SentenceSplitter(chunk_size=1024),{})[1]
    assert nodes and any('U-15' in n['text'] and '23' in n['text'] for n in nodes)
    assert path.read_bytes()==data


def test_empty_or_broken_table_never_becomes_warning_evidence(tmp_path):
    from backend.converter import _parse_spreadsheet
    empty=tmp_path/'empty.csv';empty.write_text('Артикул,Остаток\n',encoding='utf-8')
    assert not _parse_spreadsheet(empty).strip()
    broken=tmp_path/'bad.xlsx';broken.write_bytes(b'not an Excel archive')
    with pytest.raises(RuntimeError,match='прочитать таблицу'): _parse_spreadsheet(broken)


def test_full_chat_fixture_and_stream_accounting():
    fixture=json.loads((Path(__file__).parent/'fixtures/rag/full-chat-v1.json').read_text(encoding='utf-8'))
    assert len(fixture['cases'])==100 and len({c['id'] for c in fixture['cases']})==100
    assert sum(d.get('format')=='scan_pdf' for d in fixture['documents'])==4
    assert sum(d.get('format') in {'csv','xlsx'} for d in fixture['documents'])==6
    assert coverage([{'doc_name':'one.md'}],['one','two'])==.5
    assert coverage([],[]) is None
    response=SimpleNamespace(iter_lines=lambda:iter(['event: error','data: {"code":"CHAT_MEMORY_PRESSURE"}','']))
    event,payload,_=stream_final(response)
    assert event=='error' and payload['code']=='CHAT_MEMORY_PRESSURE'


def test_answer_footer_surfaces_observed_weak_search_without_claiming_truth():
    from sovushka.components.chat_presentation import _operator_status_chips
    chips=_operator_status_chips('MODEL_OUTPUT',{'retrieval_trace':{'quality_status':'weak'}},['a.md'])
    assert any(c['label']=='Поиск дал слабые совпадения' and c['tone']=='warn' for c in chips)
    assert chips[0]['tone']=='muted'
    ordinary=_operator_status_chips('MODEL_OUTPUT',{'retrieval_trace':{'quality_status':'good'}},['a.md'])
    assert not any(c['tone']=='warn' for c in ordinary)


@pytest.mark.parametrize('cached,fresh,allowed', [(1,12,True),(12,1,False)])
def test_admission_measures_recovered_or_new_pressure(monkeypatch,cached,fresh,allowed):
    from proxy.services.runtime_admission import live_memory_metrics, evaluate_chat_admission
    from proxy.services.model_resource_service import ModelResourceTarget
    monkeypatch.setattr('backend.system_memory.system_memory_snapshot',
        lambda:{'ram_free_gb':fresh,'swap_pct':0})
    old={'ram_free_gb':cached,'swap_pct':0}
    measured=live_memory_metrics(old)
    assert old['ram_free_gb']==cached  # Dashboard data stays separate.
    decision=evaluate_chat_admission(current_mode={'mode':'chat'},metrics_cache=measured,
        connection=ModelResourceTarget('ollama','trial','trial:r1','loopback'))
    assert decision.allowed is allowed
    assert decision.indexing_chat_policy['hard_min_free_gb']==4


def test_trial_waits_for_allowed_admission_and_stops_at_deadline(monkeypatch):
    from tools import evaluate_light_chat as trial
    ticks=iter([0,0,1,2,3,4])
    monkeypatch.setattr(trial.time,'monotonic',lambda:next(ticks))
    monkeypatch.setattr(trial.time,'sleep',lambda _:None)
    answers=iter([False,True])
    api=SimpleNamespace(get=lambda path:SimpleNamespace(
        raise_for_status=lambda:SimpleNamespace(json=lambda:{'chat_generation_allowed':next(answers)})))
    assert trial.wait_admission(api)==1
    ticks=iter([0,1,2])
    monkeypatch.setattr(trial.time,'monotonic',lambda:next(ticks))
    blocked=SimpleNamespace(get=lambda path:SimpleNamespace(
        raise_for_status=lambda:SimpleNamespace(json=lambda:{'chat_generation_allowed':False})))
    with pytest.raises(ValueError,match='protection remains enabled'):
        trial.wait_admission(blocked,timeout=1)
