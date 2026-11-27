"""Resource deferral, exact table evidence and multilingual source fidelity."""
from io import BytesIO
import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from backend.document_catalog import MetaDB
from backend.interface import Chunk
from backend.qdrant_ingestion import QdrantIngestion
from backend.qdrant_nodes import QdrantNodes
from backend import qdrant_support as support
from proxy.services.chat_prompt_support import clean_visible_text, source_excerpts
from proxy.services.kot_service import analyze_question
from proxy.services.lexical_index_service import RetrievalTrace
from proxy.services.retrieval_quality_service import evaluate_retrieval_quality
from proxy.services.saferag_service import build_context, source_map_for_context
from tools.evaluate_light_chat import coverage, materialize, stream_final


def test_llamaindex_trial_preserves_the_received_source_order_and_transport():
    from unittest.mock import patch

    import httpx
    from llama_index.core import Settings
    from llama_index.core.schema import NodeWithScore, TextNode
    from tools.evaluate_llamaindex_folder import original_sources, render

    documents = {'b': 'Blue record 27.', 'a': 'Amber record 14.', 'z': 'Not delivered.'}
    wire = {'visible': ['b', 'a'], 'model': 'fixture', 'max_tokens': 512,
            'temperature': 0.1, 'reasoning_effort': 'none'}
    row = {'case': 'inspection', 'variant': 'missing', 'question': 'What is the amber value?', 'calls': [wire]}

    def respond(request):
        body = json.loads(request.content)
        prompt = body['messages'][-1]['content']
        assert 'Source 1:\nBlue record 27.' in prompt and 'Source 2:\nAmber record 14.' in prompt
        assert row['question'] in prompt and documents['z'] not in prompt
        assert body['model'] == 'fixture' and body['max_tokens'] == 512 and body['temperature'] == 0.1
        assert body['reasoning_effort'] == 'none'
        prompts.append(prompt)
        return httpx.Response(200, json={'choices': [{'message': {'content': '14 [2].'}, 'finish_reason': 'stop'}]})

    prompts = []
    with patch.object(Settings, '_tokenizer', str.split), httpx.Client(
        base_url='http://127.0.0.1/v1', transport=httpx.MockTransport(respond)
    ) as client:
        for mode in ('citations', 'les-citations'):
            result = render(row, documents, client, mode)
            assert result['answer'] == '14 [2].' and result['source_ids_by_number'] == {1: 'b', 2: 'a'}
            assert result['complete_source_map'] and len(result['calls']) == 1
    assert '[[NO_EVIDENCE]]' not in prompts[0] and '[[NO_EVIDENCE]]' in prompts[1]
    wrong = NodeWithScore(node=TextNode(text='Source 1:\nAmber record 14.', metadata={'original_id': 'a'}))
    with pytest.raises(ValueError, match='Conflicting citation numbers'):
        original_sources([wrong], wire['visible'], documents)


@pytest.mark.parametrize('fault', [None, 'unavailable', 'unknown_basis', 'altered_review', 'false_conflict'])
def test_part_reader_checks_scope_editions_and_exact_quotes(fault):
    from unittest.mock import patch
    import httpx
    from llama_index.core import Settings
    from tools.evaluate_llamaindex_folder import render

    documents = {'old': 'Archived. Cyan lasts 27 days.', 'new': 'Amber lasts 14 working days.'}
    wire = {'visible': ['old', 'new'], 'model': 'fixture', 'max_tokens': 512,
            'temperature': 0.1, 'reasoning_effort': 'none'}
    row = {'case': 'term', 'variant': 'missing', 'question': 'What are the current amber and cyan terms?',
           'calls': [wire]}

    def respond(request):
        body = json.loads(request.content)
        inputs = json.loads(body['messages'][-1]['content'])
        structured = body['response_format']['json_schema']
        stage = structured['name']
        if stage == 'parts':
            assert inputs == {'question': row['question']}
            output = {'properties': [{'subject_property': 'Term', 'scopes': ['Amber', 'Cyan'], 'period': 'current'}]}
        elif stage == 'editions':
            output = {'exclude': [{'source': 1, 'basis': '9.1' if fault == 'unknown_basis' else '1.1'}]}
        elif stage == 'read':
            assert inputs['sources'] == {'2.1': documents['new']}
            assert structured['schema']['properties']['excerpts']['items']['enum'] == ['2.1']
            if inputs['target']['scope'] == 'Cyan':
                output = {'status': 'missing', 'excerpts': []}
            else:
                output = {'status': 'conflict' if fault == 'false_conflict' else 'supported',
                          'excerpts': ['1.2' if fault == 'unavailable' else '2.1']}
        else:
            assert stage == 'review' and set(inputs['pairs']) == {'1/2.1'}
            output = {'accepted': ['2/1.2' if fault == 'altered_review' else '1/2.1']}
        return httpx.Response(200, json={'choices': [{'message': {'content': json.dumps(output)}, 'finish_reason': 'stop'}]})

    with patch.object(Settings, '_tokenizer', str.split), httpx.Client(
        base_url='http://127.0.0.1/v1', transport=httpx.MockTransport(respond)
    ) as client:
        result = render(row, documents, client, 'parts')
    if fault:
        assert 'error' in result and 'answer' not in result
    else:
        assert result['complete_source_map'] and result['source_ids_by_number'] == {1: 'old', 2: 'new'}
        assert 'Amber lasts 14 working days. [2]' in result['answer'] and '27' not in result['answer']
        assert 'Cyan — В предоставленных материалах сведений нет.' in result['answer']
        assert [c['stage'] for c in result['calls']] == ['parts', 'editions', 'read', 'read', 'review']


def test_folder_replay_keeps_qualifications_and_rebinds_numbered_sources():
    from tools.evaluate_folder_evidence import read_selection, variants
    import random

    fixture = json.loads((Path(__file__).parent/'fixtures/rag/folder-chat-v1.json').read_text(encoding='utf-8'))
    docs = {doc['id']: doc for doc in fixture['documents']}
    assert len(docs) == 60
    assert materialize(docs['032']) == ('032.txt', docs['032']['text'].encode('utf-8'))
    case = fixture['cases'][0]
    controls = {label: (visible, gold) for label, visible, gold in variants(case, random.Random(29))}
    assert controls['original'][1] == ['017', '056', '032']
    visible, expected = controls['without_032']
    assert '032' not in visible and expected == ['017', '056']
    assert set(controls['all_support_removed'][0]).isdisjoint(case['gold_documents'])
    assert controls['all_support_removed'][1] == []
    for label in ('original', 'shuffled'):
        addresses = {f'S{i}P1': doc for i, doc in enumerate(controls[label][0], 1)}
        refs = [key for key, doc in addresses.items() if doc in case['gold_documents']]
        reply = {'finish_reason': 'stop', 'message': {'content': json.dumps({'rows': refs})}}
        assert set(read_selection(reply, addresses)) == set(case['gold_documents'])
        with pytest.raises(ValueError):
            read_selection({**reply, 'finish_reason': 'length'}, addresses)
        with pytest.raises(ValueError):
            read_selection(reply, {})


def test_native_trial_does_not_score_a_matching_number_with_the_wrong_source_as_correct():
    from tools.evaluate_light_chat import native_stages
    expected={'index':1,'doc_name':'approved.md','page':3}
    wrong={'index':2,'doc_name':'proposal.md','page':1}
    final={'answer':'Quantity 17 [Источник 2]','source_map':[expected,wrong],'model':{'model_id':'fixture'},
        'retrieval_trace':{'engine':'rust.chat','prefetch':{'candidate_documents':[{'file_name':'approved.md'}]},
            'contexts':[{'delivery':'prepared','sources':[wrong]},
                        {'delivery':'confirmed','sources':[expected,wrong]}],
            'citation_syntax':{'indexes':[2]}}}
    result=native_stages(final,['approved'])
    assert result['candidate_gold_coverage']==result['delivered_gold_coverage']==1
    assert result['cited_gold_coverage']==0
    assert result['engine']=='rust.chat' and result['observed_model']=='fixture'
    assert result['semantic_correctness'] is None
    assert result['delivered_pages']==[expected,wrong]
    assert native_stages({},['approved'])['delivered_gold_coverage'] is None
    assert native_stages({'model':'legacy','model_connection':{'model_id':'older'}},[])['observed_model']=='older'


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
