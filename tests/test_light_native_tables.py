"""Native normalized rows and bidirectional Parquet compatibility."""
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess

import pytest
import pyarrow as pa
import pyarrow.parquet as pq
from backend import native_core
from backend.native_table_rows import normalize,chunks
from backend.native_parquet import write,read_rows
from tests.oracles import table_rows_828 as old


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):pytest.skip('Native tables ABI required')
    assert native_core.supports('tables')


def call(action,**kwargs):return native_core.required('tables',request={'action':action,**kwargs})


def test_normalization_and_chunk_content_match_previous_generic_tables():
    rng=random.Random(829)
    for i in range(300):
        raw={'Код':f'ЩР-{i} 日本語 🌲','Количество':rng.choice([None,0,2.5,'1 230,25','не число']),
            'Наименование':'Кабель Ω / м²','Единица':'м','Колонка без правила':'Источник целиком'}
        mapping={'Код':'code','Количество':'qty','Наименование':'name','Единица':'unit','Колонка без правила':'skip'}
        expected=old.normalize_row(raw,mapping,'SPEC','Книга / Лист 2','Книга.xlsx')
        value=normalize(raw,mapping,'SPEC','Книга / Лист 2','Книга.xlsx')
        assert value==expected
        for sheet in (False,True):
            row=dict(value)
            if sheet:row.update(sheet_name='Лист 2',source_row=i+2)
            assert chunks([row],'ds')==old.rows_to_qdrant_chunks([row],'ds')


def test_source_identity_cannot_be_replaced_by_column_mapping():
    value=normalize({'evil':'another.xlsx','Ноль':'0'},{'evil':'source_file','Ноль':'qty'},'TABLE','Original','source.xlsx')
    assert value['source_file']=='source.xlsx' and value['qty']==0


def test_source_column_order_and_long_raw_evidence_are_retained():
    raw={'Я последний по алфавиту':'первый в исходнике','А первый':'второй в исходнике','Много':'Ω 🌲 '*500}
    value=normalize(raw,{},'TABLE','Книга','source.xlsx');value['sheet_name']='Лист 1'
    assert list(json.loads(value['raw_row']))==list(raw)
    assert chunks([value])==old.rows_to_qdrant_chunks([value])


def test_nonfinite_quantities_do_not_become_values():
    for text in ('nan','NaN','inf','-inf','1e9999'):
        assert normalize({'n':text},{'n':'qty'},'TABLE','d','d.xlsx')['qty'] is None


def test_exact_sum_preserves_zero_and_reports_missing_values():
    value=call('sum_rows',field='amount',rows=[{'amount':0,'qty':100},{'amount':0,'qty':200}])
    assert value['exact_total']=='0' and value['count']==2 and value['complete']
    value=call('sum_rows',field='qty',rows=[{'qty':'.1'}]*600)
    assert value['exact_total']=='60' and value['total']==60
    value=call('sum_rows',field='qty',rows=[{'qty':1},{'qty':None},{'qty':'inf'},{'qty':True}])
    assert value['exact_total']=='1' and value['missing']==3 and not value['complete']


def test_saved_table_sum_does_not_change_a_zero_amount_to_quantity(tmp_path):
    from backend import native_parquet
    root=tmp_path/'storage';folder=root/'ds/_parquet';folder.mkdir(parents=True)
    path=folder/'book.parquet';native_parquet.write([{'name':'Кабель','amount':0,'qty':100,'source_file':'book.xlsx'}],path)
    value=native_parquet.sum_rows(native_parquet.read_rows(path),'amount')
    assert value['total']==0 and value['exact_total']=='0' and value['count']==1 and value['complete']


def sample():
    return [{'doc_type':'TABLE','name':'Кабель Ω 🌲','qty':0.0,'source_row':2,'needs_ocr':False,'raw_row':'{"Код": "東京"}','empty':None},
        {'doc_type':'TABLE','name':'Текст\nс переносом и |','qty':.125,'source_row':5,'needs_ocr':True,'raw_row':'{}','empty':None}]


def test_rust_snappy_output_is_readable_by_pyarrow(tmp_path):
    rows=sample();path=tmp_path/'Таблица с пробелами.parquet'
    assert write(rows,path)==2
    assert path.read_bytes().startswith(b'PAR1') and path.read_bytes().endswith(b'PAR1')
    table=pq.read_table(path)
    assert table.to_pylist()==rows and read_rows(path)==rows
    metadata=pq.read_metadata(path)
    assert metadata.created_by=='LES Rust tables v1'
    assert all(metadata.row_group(0).column(i).compression=='SNAPPY' for i in range(metadata.num_columns))
    assert not list(tmp_path.glob('.les-parquet-*'))


def test_existing_pyarrow_file_is_read_without_pandas(tmp_path,monkeypatch):
    import sys
    path=tmp_path/'old.parquet';rows=sample();pq.write_table(pa.Table.from_pylist(rows),path,compression='snappy')
    before=path.read_bytes();monkeypatch.setitem(sys.modules,'pandas',None)
    assert read_rows(path)==rows and path.read_bytes()==before


def test_nullable_rows_and_large_signed_unsigned_integers_remain_exact(tmp_path):
    for value in (2**63-1,2**64-1,-2**63):
        path=tmp_path/f'{value}.parquet';rows=[{'id':value},{'id':None}]
        assert write(rows,path)==2
        assert read_rows(path)==rows and pq.read_table(path).to_pylist()==rows


def test_unsafe_float_promotion_refuses_instead_of_rounding_an_integer(tmp_path):
    path=tmp_path/'mixed.parquet'
    with pytest.raises(native_core.NativeCoreError,match='PARQUET_NUMBER_PRECISION'):
        write([{'qty':2**60},{'qty':.5}],path)
    assert not path.exists()


def test_pagination_binds_to_the_same_file_bytes(tmp_path):
    path=tmp_path/'pages.parquet';rows=[{'name':str(i),'qty':i} for i in range(2100)];write(rows,path)
    first=call('read',path=str(path),limit=2000)
    assert first['row_count']==2100 and first['next_offset']==2000 and not first['complete']
    next_value=call('read',path=str(path),offset=2000,source_sha256=first['source_sha256'])
    assert next_value['complete'] and next_value['rows']==rows[2000:]
    write([{'name':'updated','qty':1}],path)
    with pytest.raises(native_core.NativeCoreError,match='PARQUET_SOURCE_CHANGED'):
        call('read',path=str(path),source_sha256=first['source_sha256'])


def test_bad_replacement_preserves_previous_complete_file(tmp_path):
    path=tmp_path/'atomic.parquet';write(sample(),path);before=path.read_bytes()
    with pytest.raises(native_core.NativeCoreError,match='PARQUET_VALUE_UNSUPPORTED'):
        write([{'mixed':'text'},{'mixed':True}],path)
    assert path.read_bytes()==before
    assert not list(tmp_path.glob('.les-parquet-*'))
    assert write([],path)==0 and path.read_bytes()==before


def test_corruption_and_codec_have_specific_failures(tmp_path):
    path=tmp_path/'broken.parquet';path.write_bytes(b'PAR1'+b'\0'*8)
    with pytest.raises(native_core.NativeCoreError,match='PARQUET_READ_FAILED'):read_rows(path)
    pq.write_table(pa.Table.from_pylist(sample()),path,compression='gzip')
    with pytest.raises(native_core.NativeCoreError,match='PARQUET_CODEC_UNSUPPORTED'):read_rows(path)


def test_native_table_error_has_human_public_message():
    from proxy.services.public_error_service import public_error_payload
    value=public_error_payload(status_code=409,detail='PARQUET_SOURCE_CHANGED')
    assert value['code']=='PARQUET_SOURCE_CHANGED' and 'Версия таблицы изменилась' in value['detail']


def test_standalone_worker_has_no_python_and_continues_after_corrupt_file(tmp_path):
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Native table executable required')
    source=tmp_path/'source.parquet';pq.write_table(pa.Table.from_pylist(sample()),source,compression='snappy')
    broken=tmp_path/'broken.parquet';broken.write_bytes(b'PAR1'+b'\0'*8)
    destination=tmp_path/'new.parquet'
    requests=[{'action':'read','path':str(source)},{'action':'read','path':str(broken)},
              {'action':'write','path':str(destination),'rows':sample()},{'action':'read','path':str(destination)}]
    frames=''.join(json.dumps({'request_id':i,'input':r})+'\n' for i,r in enumerate(requests))
    env=os.environ.copy();env.update(PATH='',PYTHONHOME=str(tmp_path/'none'),PYTHONPATH=str(tmp_path/'none'))
    response=subprocess.run([program,'--serve-tables'],input=frames,text=True,encoding='utf-8',capture_output=True,env=env,timeout=30)
    assert response.returncode==0,response.stderr
    values=[json.loads(line) for line in response.stdout.splitlines()]
    assert [v['result']['status'] for v in values]==['completed','failed','completed','completed']
    assert values[0]['result']['value']['rows']==sample()==values[-1]['result']['value']['rows']
    assert 'table_operation_failed' in response.stderr
