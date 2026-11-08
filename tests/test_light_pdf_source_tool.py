"""Physical page identity, revision and error boundaries, using the real Rust worker."""
import hashlib
import os
import sqlite3
from pathlib import Path
from types import SimpleNamespace

import pytest

from backend import native_core
from backend.pdf_reader import read_source_page
from proxy.services import tool_harness_service as tools
from proxy.services.chat_prompt_support import _augment_model_tool_args
from proxy.services.pdf_source_evidence_service import require_source_read_success, physical_page_chunks, require_complete_pdf_reads
from proxy.services.chat_section_context_service import SectionReadError
from proxy.services.public_error_service import public_error_payload
from tests.pdf_fixtures import write_pdf


@pytest.fixture
def physical_pdf(tmp_path, monkeypatch):
    import pypdfium2_raw
    worker = os.getenv('LES_TEST_NATIVE_API')
    if not worker or not native_core.supports('worker_pdf'):
        pytest.skip('Native PDF worker required')
    monkeypatch.setenv('LES_NATIVE_PDF_WORKER', worker)
    monkeypatch.setenv('LES_NATIVE_PDFIUM', str(Path(pypdfium2_raw.__file__).with_name('pdfium.dll')))
    path = write_pdf(tmp_path/'Русский путь 東京.pdf', ['First page', 'True physical second page ABCDEFGHIJKLMNOP', None])
    database = tmp_path/'meta.db'
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    with sqlite3.connect(database) as conn:
        conn.execute('CREATE TABLE documents(id TEXT,dataset_id TEXT,file_name TEXT,source_path TEXT,file_hash TEXT)')
        conn.execute('INSERT INTO documents VALUES(?,?,?,?,?)', ('actual-id','dataset','folder/Doc.pdf',str(path),digest))
    monkeypatch.setattr(tools, 'explorer', lambda: SimpleNamespace(path=database))
    yield path, database, digest
    native_core.required('worker_stop')


def test_native_original_not_index_and_exact_page_with_cursor(physical_pdf):
    path, database, digest = physical_pdf
    before=path.read_bytes()
    args={'doc_id':'actual-id','page':2,'max_chars':12,'source_sha256':digest}
    first=tools._tool_read_pdf_source(args)
    assert first['status']=='ok'
    assert first['result']['text']=='True physica'
    assert first['result']['complete'] is False and first['result']['next_offset']==12
    with pytest.raises(SectionReadError,match='PDF_PAGE_INCOMPLETE'):
        require_complete_pdf_reads([first])
    args['offset']=12;args['max_chars']=12000
    second=tools._tool_read_pdf_source(args)
    assert first['result']['text']+second['result']['text']=='True physical second page ABCDEFGHIJKLMNOP'
    chunk,=physical_page_chunks(second)
    assert chunk.meta['page']==2 and chunk.meta['source_sha256']==digest
    assert second['result']['doc_id']=='actual-id'
    require_source_read_success(second)
    require_complete_pdf_reads([first,second])
    assert path.read_bytes()==before


@pytest.mark.parametrize('change,code', [
    ({'page':999},'PDF_PAGE_OUT_OF_RANGE'),
    ({'page':3},'PDF_OCR_REQUIRED'),
    ({'doc_id':'Doc.pdf'},'PDF_DOCUMENT_NOT_FOUND'),
    ({'dataset_id':'wrong'},'PDF_DOCUMENT_NOT_FOUND'),
    ({'source_sha256':'0'*64},'PDF_REVISION_CHANGED'),
    ({'q':'Definitely not present'},'PDF_QUERY_NOT_FOUND'),
])
def test_read_failure_has_no_evidence_and_blocks_final_answer(physical_pdf,change,code):
    _,_,digest=physical_pdf
    args={'doc_id':'actual-id','page':2,'source_sha256':digest,**change}
    result=tools._tool_read_pdf_source(args)
    assert result['status']=='error' and result['result']['code']==code
    assert result['sources']==[] and physical_page_chunks(result)==()
    with pytest.raises(SectionReadError) as error:
        require_source_read_success(result)
    public=public_error_payload(status_code=error.value.status_code,detail=error.value.detail)
    assert public['code']==code and 'PDF_' not in public['detail']


def test_file_changed_after_index_is_not_read_as_old_revision(physical_pdf):
    path,_,digest=physical_pdf
    write_pdf(path,['Different revision'])
    result=tools._tool_read_pdf_source({'doc_id':'actual-id','page':1,'source_sha256':digest})
    assert result['result']['code']=='PDF_REVISION_CHANGED' and not result['sources']


def test_uploaded_original_uses_owned_storage_and_exact_catalog_identity(physical_pdf, monkeypatch):
    path, database, digest = physical_pdf
    state = database.parent/'owned-state'
    original = state/'storage/datasets/dataset/folder/Doc.pdf'
    original.parent.mkdir(parents=True)
    original.write_bytes(path.read_bytes())
    monkeypatch.setenv('LES_STATE_ROOT', str(state))
    with sqlite3.connect(database) as conn:
        conn.execute("UPDATE documents SET source_path='' WHERE id='actual-id'")
    answer = read_source_page(str(database), {'doc_id':'actual-id','page':2,'source_sha256':digest})
    assert answer['status']=='completed'
    assert answer['value']['text']=='True physical second page ABCDEFGHIJKLMNOP'
    assert answer['value']['doc_id']=='actual-id' and answer['value']['page']==2
    # A model-supplied storage root is ignored: only the server supplies it.
    answer = read_source_page(str(database), {'doc_id':'actual-id','page':2,'content_root':str(database.parent)})
    assert answer['status']=='completed'
    with sqlite3.connect(database) as conn:
        conn.execute("UPDATE documents SET file_name='../Doc.pdf' WHERE id='actual-id'")
    answer = read_source_page(str(database), {'doc_id':'actual-id','page':2})
    assert answer['code']=='PDF_SOURCE_INVALID'


def test_literal_query_returns_verified_page_passage(physical_pdf):
    _,_,digest=physical_pdf
    result=tools._tool_read_pdf_source({'doc_id':'actual-id','page':2,'source_sha256':digest,
        'q':'SECOND page','max_chars':30})
    assert result['status']=='ok' and result['result']['selection']=='literal_match'
    assert 'second page' in result['result']['text'] and result['result']['page']==2
    require_complete_pdf_reads([result])  # Explicit passage lookup does not claim full-page coverage.


def test_identity_binding_uses_unique_selected_provenance(physical_pdf):
    _,_,digest=physical_pdf
    call=_augment_model_tool_args({'tool':'read_pdf_source','args':{'doc_id':'Doc.pdf','dataset_id':'folder','page':2}},
        question='Read page',dataset_ids=['dataset'],target_file_ref={
            'match_status':'matched','dataset_id':'dataset','file_name':'folder/Doc.pdf'},
        sources=[{'dataset_id':'dataset','doc_name':'folder/Doc.pdf','doc_id':'opaque-point-id','source_sha256':digest}])
    assert call['args']['dataset_id']=='dataset' and call['args']['doc_name']=='folder/Doc.pdf'
    assert 'doc_id' not in call['args'] and call['args']['source_sha256']==digest
    result=tools._tool_read_pdf_source(call['args'])
    assert result['result']['doc_id']=='actual-id' and result['result']['page']==2


def test_missing_read_is_not_a_successful_tool_result():
    if not native_core.supports('harness_evidence'):
        pytest.skip('Native harness required')
    with pytest.raises(SectionReadError,match='SOURCE_READ_FAILED'):
        require_source_read_success({'tool':'read_source','status':'missing','result':{}})


def test_pdf_schema_requires_physical_page():
    schema=tools._canonical_input_schema('read_pdf_source',{'page':'int','offset':'int','doc_id':'str'})
    assert schema['required']==['page'] and schema['properties']['page']['minimum']==1
