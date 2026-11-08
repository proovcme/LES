"""Source/page identity, replay, budgets and IO-only boundaries across the real ABI."""
import json
import os
from types import SimpleNamespace
import uuid

import pytest

from backend import native_core, qdrant_support as support
from backend.qdrant_nodes import QdrantNodes
from backend.extraction_errors import DocumentExtractionError
from backend.source_nodes import prepare


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Native source node ABI required')
    assert native_core.supports('document_source_nodes')


def pdf(text, **kwargs):
    return prepare(text,dataset_id='dataset_日本語',file_key='Раздел/Книга #1.pdf',
                   pdf=True,size=800,overlap=150,**kwargs)


def test_pdf_parts_keep_legacy_uuid_physical_pages_and_exact_overlap():
    first='Начало страницы 😀 日本語. '+'Продолжение со значениями м² и ЩР-1. '*90
    second='Другой лист: окончание условия, 25 кВт.'
    value=pdf('## Стр. 1\n\n'+first+'\n\n## Page 2\n\n'+second)
    parts=QdrantNodes._split_pdf_page_text(first,max_chars=800,overlap=150)
    assert len(value)==len(parts)+1
    for index,part in enumerate(parts,1):
        node=value[index-1]
        assert node['text']==f'## Page 1 part {index}/{len(parts)}\n\n'+part
        assert node['doc_id']==str(uuid.uuid5(uuid.NAMESPACE_URL,
            f'dataset_日本語:Раздел/Книга #1.pdf:pdf-page:1:{index}'))
        assert node['payload']=={'type':'pdf_page_text','source_layer':'pdf_text_layer',
            'page':1,'page_part':index,'page_parts':len(parts),'source_ref':'Раздел/Книга #1.pdf#page=1',
            'source_node_builder':'rust.document.source-nodes.v1'}
    assert value[-1]['text']=='## Page 2\n\n'+second
    assert value[-1]['payload']['page']==2
    assert all(second not in n['text'] for n in value[:-1])


def test_pdf_short_pages_empty_markers_and_passport_provenance_survive():
    seen=[]
    fragments=[{'fragment_id':f'part-{i}','bbox':[0,i,20,30],'source_ref':'trusted'} for i in range(7)]
    def passports(pages):
        seen.extend(pages)
        return {7:{'schema':'v2','page_type':'drawing','page_type_label':'Чертёж',
            'routing_confidence':.87,'requires_ocr':True,'recognition_quality':{'confidence':.91},
            'signals':{'title_block':True},'stamp':{'status':'found','sheet_number':'ЭОМ-7'},
            'evidence_fragments':fragments,'source_ref':'doc.pdf#page=7'}}
    value=pdf('## Page 5\n\n[no text extracted]\n\n## Page 6\n\n\n## Page 7\n\nЩР-7',
        passport_reader=passports,route=SimpleNamespace(metadata={'domain':'PROJECT'},pipeline='markdown'))
    assert seen==[5,6,7] and len(value)==1
    meta=value[0]['payload']
    assert meta['source_layer']=='pdf_ocr_text' and meta['page']==7
    assert meta['pdf_sheet_number']=='ЭОМ-7' and meta['pdf_page_type']=='drawing'
    assert meta['pdf_recognition_quality']=={'confidence':.91}
    assert meta['pdf_fragment_bboxes']==fragments[:5]
    assert meta['source_ref']=='doc.pdf#page=7' and meta['domain']=='PROJECT'


def test_optional_passport_failure_keeps_text_and_logs_reason(caplog):
    def fail(_):raise RuntimeError('contour reader unavailable')
    value=pdf('## Page 1\n\nПодтверждённый текст',passport_reader=fail)
    assert value[0]['text'].endswith('Подтверждённый текст')
    assert 'contour reader unavailable' in caplog.text


def test_classification_cannot_overwrite_physical_page_or_source_identity():
    value=pdf('## Page 7\n\nРеальное доказательство',route=SimpleNamespace(
        metadata={'page':999,'type':'other','source_ref':'wrong.pdf#page=999','domain':'PROJECT'},
        pipeline='markdown'))
    meta=value[0]['payload']
    assert meta['page']==7 and meta['type']=='pdf_page_text'
    assert meta['source_ref']=='Раздел/Книга #1.pdf#page=7' and meta['domain']=='PROJECT'


def test_duplicate_page_markers_stop_before_emitting_any_node():
    calls=[]
    def exchange(operation,value):
        calls.append(operation)
        return {'text':'## Page 1\n\nПервое\n\n## Page 1\n\nВторое','complete':True}
    with pytest.raises(native_core.NativeCoreError,match='PDF_PAGE_DUPLICATE'):
        native_core.run_source_nodes({'schema':'les.document-source-nodes.v1','kind':'pdf',
            'dataset_id':'data','file_key':'a.pdf','size':800,'overlap':150},exchange)
    assert calls==[0]


def test_markdown_native_sections_match_previous_parser_text_and_have_stable_ids():
    text='# Раздел 日本語\n\n'+('Условие 😀 м². '*20)+'\n\n## Следующий\n\n'+('Другой пункт. '*12)
    expected=[n.text for n in support.MarkdownNodeParser().get_nodes_from_documents([support.Document(text=text)])]
    kwargs=dict(dataset_id='data',file_key='Документ.md',size=500,overlap=50)
    first=prepare(text,**kwargs)
    assert [n['text'] for n in first]==expected
    assert prepare(text,**kwargs)==first
    assert len({n['doc_id'] for n in first})==len(first)
    assert prepare(text,**{**kwargs,'file_key':'Другая редакция.md'})!=first


def test_markdown_short_standalone_evidence_is_kept_but_short_heading_is_filtered():
    assert prepare('Короткая самостоятельная запись с доказательством.',
        dataset_id='d',file_key='note.txt',size=300,overlap=30)
    text='# Кратко\n\n## Большой раздел\n\n'+('Длинный полезный текст. '*30)
    nodes=prepare(text,dataset_id='d',file_key='note.md',size=300,overlap=30)
    assert all(n['text']!='# Кратко' for n in nodes)


def test_native_selected_tokenizer_does_not_call_python_length_function(tmp_path,monkeypatch):
    asset=tmp_path/'tokenizer.json'
    asset.write_text(json.dumps({'version':'1.0','truncation':None,'padding':None,'added_tokens':[],
        'normalizer':None,'pre_tokenizer':{'type':'Whitespace'},'post_processor':None,'decoder':None,
        'model':{'type':'WordLevel','vocab':{'[UNK]':0,'word':1,'данные':2,'.':3},'unk_token':'[UNK]'}}),encoding='utf-8')
    selected=native_core.LocalTokenizer(asset)
    text='# Структура\n\n'+('word данные 😀. '*300)
    value=prepare(text,dataset_id='d',file_key='a.md',size=30,overlap=3,
        unit='tokens',tokenizer=selected.asset,minimum=20)
    assert len(value)>1
    expected=support.StructureAwareSplitter(chunk_size=30,chunk_overlap=3,len_fn=selected)._split(text)
    assert [n['text'] for n in value]==[v for v in expected if len(v)>=20]


def test_canonical_markdown_route_no_longer_constructs_llama_nodes(monkeypatch,tmp_path):
    text='# Заголовок\n\n'+'Текст с достаточным размером. '*15
    monkeypatch.setattr(support,'convert_to_markdown_for_indexing',lambda *a,**k:text)
    monkeypatch.setattr(support.MarkdownNodeParser,'get_nodes_from_documents',
        lambda *a,**k:pytest.fail('Python parser called'))
    splitter=support.StructureAwareSplitter(chunk_size=300,chunk_overlap=30)
    value=QdrantNodes()._sync_markdown_nodes(tmp_path/'a.txt','a.txt','d',
        support.MarkdownNodeParser(),splitter)
    assert value and all(n['payload']['type']=='markdown' for n in value)


def test_large_text_uses_bounded_input_and_output_instead_of_generic_abi_limit():
    text=('日本語😀 и кириллица. '*240000)+'\n\nОкончание документа.'
    assert len(text.encode())>8*1024*1024
    value=prepare(text,dataset_id='d',file_key='large.md',size=12000,overlap=300)
    assert value and 'Окончание документа.' in value[-1]['text']
    assert sum(len(n['text']) for n in value)>=len(text)


def test_missing_tokenizer_does_not_switch_to_character_budgets():
    with pytest.raises(DocumentExtractionError) as fault:
        prepare('Непустой документ. '*200,dataset_id='d',file_key='a.md',size=30,overlap=3,
                unit='tokens')
    assert fault.value.code=='DOCUMENT_TOKENIZER_UNAVAILABLE'


def test_error_code_survives_format_adapter_instead_of_becoming_generic_failure():
    with pytest.raises(DocumentExtractionError) as fault:
        pdf('## Page 1\n\nПервое\n\n## Page 1\n\nВторое')
    assert fault.value.code=='PDF_PAGE_DUPLICATE'
    assert 'повторился номер страницы' in str(fault.value)


def test_short_physical_page_survives_final_budgets_and_parent_preparation():
    raw=pdf('## Page 7\n\nЩР-7')
    result=native_core.required('document',request={'action':'prepare_nodes','nodes':raw,
        'dataset_id':'d','file_name':'drawing.pdf','window':4,
        'config':{'unit':'chars','size':1200,'minimum':20,'tokenizer':None}})['nodes']
    evidence=[n for n in result if n['payload']['node_role']=='evidence']
    assert len(evidence)==1 and 'ЩР-7' in evidence[0]['text']
    assert evidence[0]['payload']['page']==7
