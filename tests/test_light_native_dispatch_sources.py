"""The dispatcher consumes immutable source bytes and emits actual document/page references."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
from zipfile import ZipFile

import pytest
from backend import native_core
from tests.test_light_native_format_dispatch import dispatch


@pytest.mark.parametrize('suffix,encoding',[('.txt','utf-8'),('.md','utf-16'),('.txt','utf-32')])
def test_unicode_dispatch_preserves_source_and_text(tmp_path,suffix,encoding):
    path=tmp_path/('Раздел Ω'+suffix);body='# Заголовок\n\nСодержание документа 東京. '*35
    path.write_bytes(body.encode(encoding));before=path.read_bytes()
    value=dispatch(path,tmp_path/'data')
    assert value['nodes'] and all(n['payload']['format_owner']=='rust.format-dispatch.v1' for n in value['nodes'])
    assert any('東京' in n['text'] for n in value['nodes'])
    assert not any('parquet_path' in n['payload'] for n in value['nodes'])
    assert path.read_bytes()==before


def test_docx_dispatch_keeps_heading_and_table_together(tmp_path):
    path=tmp_path/'Документ.docx'
    xml='''<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>1. Заголовок</w:t></w:r></w:p>
    <w:p><w:r><w:t>Полное описание документа, которое должно остаться вместе с его таблицей.</w:t></w:r></w:p>
    <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Позиция</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Значение</w:t></w:r></w:p></w:tc></w:tr>
    <w:tr><w:tc><w:p><w:r><w:t>東京</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>12,5</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>'''
    with ZipFile(path,'w') as book:book.writestr('word/document.xml',xml)
    before=path.read_bytes();value=dispatch(path,tmp_path/'data');text='\n'.join(n['text'] for n in value['nodes'])
    assert 'Заголовок' in text and '東京' in text and '12,5' in text and path.read_bytes()==before


def test_invalid_json_never_yields_partial_nodes_or_overwrites_output(tmp_path):
    path=tmp_path/'source.jsonl';path.write_text('{"text":"first complete record"}\n{"broken":',encoding='utf-8')
    spool=tmp_path/'private.nodes'
    with pytest.raises(native_core.NativeCoreError,match='JSON_INVALID'):dispatch(path,tmp_path/'data',node_spool=str(spool))
    assert not spool.exists()


def test_external_source_plan_does_not_require_or_scan_derivative_directory(tmp_path):
    import sqlite3
    from tests.test_light_native_index_batch import make_case
    request=make_case(tmp_path/'state');path=tmp_path/'Readonly source.txt';path.write_text('External source',encoding='utf-8')
    with sqlite3.connect(request['database']) as db:db.execute('UPDATE documents SET source_path=?',(str(path),))
    missing=tmp_path/'not-created/ds'
    value=native_core.required('index_sources',request={'database':request['database'],'root':str(missing),'dataset_id':'ds','limit':100})
    assert value['files'][0]['path']==str(path) and value['files'][0]['error'] is None
    assert not missing.exists()


def test_pdf_dispatch_native_worker_reads_both_real_pages_without_python(tmp_path):
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Native API required')
    import pypdfium2_raw
    from tests.pdf_fixtures import write_pdf
    path=tmp_path/'Document with spaces.pdf';pages=[]
    for page in (1,2):
        pages.append('\n'.join(f'BT /F1 5 Tf 10 {280-row*7} Td (Page {page}; physical row {row}: section boundary evidence and source document text.) Tj ET' for row in range(35)).encode('ascii'))
    write_pdf(path,pages);before=path.read_bytes()
    request={'schema':'les.document-convert.v1','pipeline':'nodes','path':str(path),'data_dir':str(tmp_path/'data'),
        'dataset_id':'ds','file_key':path.name,'source_sha256':hashlib.sha256(before).hexdigest(),
        'chunking':{'unit':'chars','chunk_size':800,'chunk_overlap':80},'minimum':20,'final_minimum':20}
    env={**os.environ,'LES_NATIVE_PDFIUM':str(Path(pypdfium2_raw.__file__).parent/'pdfium.dll'),
         'PATH':str(tmp_path/'no-programs'),'PYTHONHOME':str(tmp_path/'no-python'),'PYTHONPATH':str(tmp_path/'no-code')}
    process=subprocess.run([program,'--serve-document-formats'],input=json.dumps({'request_id':1,'input':request})+'\n',
        capture_output=True,text=True,encoding='utf-8',env=env,timeout=30)
    assert process.returncode==0,process.stderr
    result=json.loads(process.stdout)['result'];assert result['status']=='completed',result
    nodes=result['value']['nodes'];assert {n['payload']['page'] for n in nodes}=={1,2}
    assert all(n['payload']['source_ref']==path.name+'#page='+str(n['payload']['page']) for n in nodes)
    assert any(n['payload']['page_part']>1 for n in nodes)
    assert any('physical row 34' in n['text'] and n['payload']['page']==2 for n in nodes)
    assert path.read_bytes()==before


@pytest.mark.skipif(os.name!='nt',reason='Windows native OCR helper contract')
def test_scan_dispatch_uses_quoted_native_settings_and_never_indexes_helper_failure(tmp_path):
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Native API required')
    import pypdfium2_raw
    from tests.pdf_fixtures import write_pdf
    root=tmp_path/'Native runtime';tools=root/'tools';tools.mkdir(parents=True)
    state=tmp_path/'state';state.mkdir()
    envfile=state/'.env';envfile.write_text("RAG_OCR_BACKEND='windows'\nRAG_OCR_DPI=\"150\"\n",encoding='utf-8')
    helper=tools/'light_windows_ocr.ps1'
    helper.write_text("[Console]::WriteLine('{\"text\":\"Controlled native OCR helper result, not an OCR quality benchmark.\"}')",encoding='utf-8')
    path=write_pdf(tmp_path/'Scan.pdf',[None]);spool=tmp_path/'failed.nodes';before=path.read_bytes()
    request={'schema':'les.document-convert.v1','pipeline':'nodes','path':str(path),'data_dir':str(tmp_path/'data'),
        'dataset_id':'ds','file_key':path.name,'chunking':{'unit':'chars','chunk_size':800,'chunk_overlap':80},'minimum':20,'final_minimum':20}
    env={**os.environ,'LES_RUNTIME_HOME':str(root),'LES_STATE_ROOT':str(state),'LES_ENV_PATH':str(envfile),
         'LES_NATIVE_PDFIUM':str(Path(pypdfium2_raw.__file__).with_name('pdfium.dll')),
         'PYTHONHOME':str(tmp_path/'no-python'),'PYTHONPATH':str(tmp_path/'no-application'),'PATH':str(tmp_path/'no-programs')}
    def invoke(settings):
        result=subprocess.run([program,'--serve-document-formats'],input=json.dumps({'request_id':1,'input':settings})+'\n',
            capture_output=True,text=True,encoding='utf-8',env=env,timeout=30)
        assert result.returncode==0,result.stderr
        return json.loads(result.stdout)['result']
    value=invoke(request);assert value['status']=='completed',value
    assert value['value']['nodes'][0]['payload']['source_layer']=='pdf_ocr_text'
    helper.write_text('exit 5',encoding='utf-8')
    failed=invoke({**request,'node_spool':str(spool)})
    assert failed['status']=='failed' and failed['code']=='OCR_READ_FAILED',failed
    assert not spool.exists() and path.read_bytes()==before
