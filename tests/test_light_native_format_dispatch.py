"""Complete table dispatch must never call the retained Python format handlers."""
import hashlib
import json
import os
from pathlib import Path
import sqlite3

import pytest
from openpyxl import Workbook
from backend import native_core
from backend.native_parquet import read_rows


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Native dispatch ABI required')
    assert native_core.supports('document_dispatch')


def source(path,count=3):
    book=Workbook();sheet=book.active;sheet.title='ЭОМ 1'
    sheet.append(['Позиция','Наименование','Количество','Ед.изм.'])
    for i in range(count):sheet.append([i+1,f'Кабель Ω 🌲 / строка {i}',i+.125,'м'])
    second=book.create_sheet('СКС 2');second.append(['Код','Наименование','Количество','Ед.изм.']);second.append(['Ш-1','Второй лист 東京',70,'шт.'])
    book.save(path)


def dispatch(path,root,**extra):
    request={'schema':'les.document-convert.v1','action':'convert','pipeline':'nodes','path':str(path),
        'data_dir':str(root),'dataset_id':'ds','file_key':'Раздел/'+path.name,
        'chunking':{'unit':'chars','chunk_size':800,'chunk_overlap':80,'tokenizer':None},
        'minimum':20,'final_minimum':20,'table_row_limit':600,**extra}
    return native_core.required('document',request=request)


def test_complete_dispatch_keeps_worksheets_rows_values_and_immutable_artifact(tmp_path):
    path=tmp_path/'Книга с пробелами.xlsx';source(path);root=tmp_path/'state/storage/datasets/ds'
    before=path.read_bytes();value=dispatch(path,root)
    assert value['owner']=='rust.format-dispatch' and value['count']==4
    payloads=[n['payload'] for n in value['nodes']]
    assert {p['sheet_name'] for p in payloads}=={'ЭОМ 1','СКС 2'}
    assert {p['row'] for p in payloads}>={2,3,4}
    assert all(p['format_owner']=='rust.format-dispatch.v1' for p in payloads)
    refs={p['parquet_path'] for p in payloads};assert len(refs)==1
    relative=refs.pop();assert relative.startswith('_parquet/.les-versions/')
    artifact=root/relative;data=read_rows(artifact);assert len(data)==4
    assert payloads[0]['parquet_sha256']==hashlib.sha256(artifact.read_bytes()).hexdigest()
    assert all(p['parquet_source_sha256']==hashlib.sha256(before).hexdigest() for p in payloads)
    again=dispatch(path,root);assert {n['payload']['parquet_path'] for n in again['nodes']}=={relative}
    assert path.read_bytes()==before


def test_replacement_has_a_new_blob_and_keeps_previous_generation_readable(tmp_path):
    path=tmp_path/'book.xlsx';source(path);root=tmp_path/'state/storage/datasets/ds'
    first=dispatch(path,root);old=first['nodes'][0]['payload'];old_bytes=(root/old['parquet_path']).read_bytes()
    source(path,count=5);second=dispatch(path,root);new=second['nodes'][0]['payload']
    assert new['parquet_path']!=old['parquet_path']
    assert (root/old['parquet_path']).read_bytes()==old_bytes
    assert len(read_rows(root/old['parquet_path']))==4 and len(read_rows(root/new['parquet_path']))==6


def test_large_table_is_navigation_projection_with_full_parquet(tmp_path):
    path=tmp_path/'rows.xlsx';source(path,30);root=tmp_path/'data'
    value=dispatch(path,root,table_row_limit=10)
    assert value['count']==1 and value['nodes'][0]['payload']['type']=='table_navigation_projection'
    assert len(read_rows(root/value['nodes'][0]['payload']['parquet_path']))==31


def test_raw_node_spool_is_created_in_rust_and_never_overwritten(tmp_path):
    path=tmp_path/'book.xlsx';source(path);spool=tmp_path/'a.nodes'
    value=dispatch(path,tmp_path/'data',node_spool=str(spool))
    assert value['node_state']=='les.nodes.raw.v1' and value['count']==4
    nodes=[json.loads(line) for line in spool.read_text(encoding='utf-8').splitlines()]
    assert len(nodes)==4 and all('_les_file_typing' in n for n in nodes)
    before=spool.read_bytes()
    with pytest.raises(native_core.NativeCoreError,match='INDEX_NODE_SPOOL_FAILED'):dispatch(path,tmp_path/'data',node_spool=str(spool))
    assert spool.read_bytes()==before


def test_source_change_and_invalid_output_key_fail_before_publishing(tmp_path):
    path=tmp_path/'book.xlsx';source(path)
    with pytest.raises(native_core.NativeCoreError,match='SOURCE_CHANGED'):dispatch(path,tmp_path/'data',source_sha256='0'*64)
    with pytest.raises(native_core.NativeCoreError,match='DOCUMENT_DISPATCH_INVALID'):dispatch(path,tmp_path/'data',file_key='../outside.xlsx')


def test_shipping_convert_does_not_enter_python_router_or_normalizer(tmp_path,monkeypatch):
    from backend import qdrant_support as support,format_process
    from backend.qdrant_ingestion import QdrantIngestion
    from backend.qdrant_nodes import QdrantNodes
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Native worker required')
    monkeypatch.setenv('LES_NATIVE_FORMAT_WORKER',program)
    monkeypatch.setattr(support,'route_document',lambda *_:pytest.fail('Python classifier called'))
    monkeypatch.setattr(support,'TableNormalizer',lambda **_:pytest.fail('Python normalizer called'))
    monkeypatch.setattr(format_process,'prepare',lambda *_:pytest.fail('Python format worker called'))
    path=tmp_path/'book.xlsx';source(path);timings={}
    route,nodes=QdrantIngestion._convert_file(QdrantNodes(),path,tmp_path/'private','book.xlsx','ds',
        support.MarkdownNodeParser(),support.StructureAwareSplitter(chunk_size=800,chunk_overlap=80),timings)
    assert route.pipeline=='parquet' and nodes and all(n['payload']['format_owner']=='rust.format-dispatch.v1' for n in nodes)
    native_core.required('worker_release_idle')


def test_native_batch_dispatch_does_not_call_python_conversion_bridge(tmp_path):
    from tests.test_light_native_index_batch import make_case,Boundary
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Native worker required')
    request=make_case(tmp_path/'state',('book.xlsx',));path=Path(request['content_root'])/'ds/book.xlsx';source(path)
    with sqlite3.connect(request['database']) as db:
        info=path.stat();db.execute('UPDATE documents SET file_size=?,file_mtime=?',(info.st_size,info.st_mtime))
    request.update(chunking={'unit':'chars','chunk_size':800,'chunk_overlap':80},
        node_preparation={'unit':'chars','size':800,'minimum':20,'tokenizer':None,'window':4},
        format_worker={'program':program,'directory':str(Path(program).parent),'timeout_ms':30_000,'minimum':20,'final_minimum':20,'table_row_limit':600})
    boundary=Boundary(request,convert=lambda *_:pytest.fail('Python conversion bridge called'))
    result=native_core.run_index_batch(request,boundary)
    assert result['files_parsed']==1 and result['errors']==0,result
    assert not any(op==0 for op,_ in boundary.events)
    assert boundary.saved and all(p['payload']['parquet_sha256'] for p in boundary.saved)
    native_core.required('worker_release_idle')


def test_unpublished_blobs_are_never_discovered_as_current_tables(tmp_path):
    path=tmp_path/'book.xlsx';source(path);root=tmp_path/'storage/ds'
    dispatch(path,root)
    value=native_core.required('tables',request={'action':'references','database':str(tmp_path/'missing.db'),
        'storage_root':str(tmp_path/'storage'),'dataset_ids':['ds']})
    assert value['refs']==[] and value['complete']
