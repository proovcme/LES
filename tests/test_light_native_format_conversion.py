"""Real Rust parsing, hostile archives and supervised worker recovery."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import zipfile

import pytest
from openpyxl import Workbook
from backend import native_core
from backend.extraction_errors import DocumentExtractionError


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Native conversion ABI required')
    assert native_core.supports('document_conversion')


def read(path, **settings):
    return native_core.required('document', request={
        'schema':'les.document-convert.v1', 'action':'convert', 'path':str(path.resolve()), **settings})


def deck(path, *, order=('r10','r2'), bad_target=False, content=None):
    with zipfile.ZipFile(path,'w',zipfile.ZIP_DEFLATED) as z:
        z.writestr('ppt/presentation.xml','<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst>'+
            ''.join(f'<p:sldId id="{i}" r:id="{key}"/>' for i,key in enumerate(order))+'</p:sldIdLst></p:presentation>')
        z.writestr('ppt/_rels/presentation.xml.rels','<Relationships>'+
            ''.join(f'<Relationship Id="r{i}" Type="x/slide" Target="'+
                ('https://example.invalid/secret' if bad_target else f'slides/slide{i}.xml')+'"/>' for i in (2,10))+'</Relationships>')
        for i in (2,10):
            z.writestr(f'ppt/slides/slide{i}.xml',content or f'''<p:sld xmlns:p="p" xmlns:a="a"><p:sp>
                <p:ph type="title"/><a:p><a:r><a:t>Слайд {i} &amp; 東京 &#x1f332;</a:t></a:r></a:p>
                <a:p><a:r><a:t>Строка</a:t></a:r><a:br/><a:r><a:t>продолжение</a:t></a:r></a:p>
                <a:p><a:r><a:t/></a:r></a:p></p:sp><a:tbl><a:tr><a:tc><a:p><a:r><a:t>Код</a:t></a:r></a:p></a:tc>
                <a:tc><a:p><a:r><a:t>Значение</a:t></a:r></a:p></a:tc></a:tr><a:tr><a:tc><a:p><a:r><a:t>ЩР|1</a:t></a:r></a:p></a:tc>
                <a:tc><a:p><a:r><a:t>25 кВт</a:t></a:r></a:p></a:tc></a:tr></a:tbl></p:sld>''')
    return path


def test_all_worksheets_dates_formulas_and_source_rows(tmp_path):
    from datetime import datetime, timedelta
    path=tmp_path/'Книга 日本語.xlsx'
    book=Workbook();first=book.active;first.title='ЭОМ 1'
    first.append(['Код','Значение','Дата','Пусто']);first.append(['ЩР|1',23,datetime(2025,10,13,12,0),None])
    first.append(['ЩР-2','=SUM(B2,1)',timedelta(hours=30),None])
    second=book.create_sheet('СКС 2');second.sheet_state='hidden'
    second.append(['Документ','Мощность']);second.append(['Другой лист 🌲',.1]);book.save(path)
    before=path.read_bytes();value=read(path)
    assert value['status']=='completed' and value['owner']=='rust.document.conversion'
    assert [s['name'] for s in value['sheets']]==['ЭОМ 1','СКС 2']
    assert 'ЩР&#124;1' in value['text'] and '2025-10-13 12:00:00' in value['text']
    assert 'Excel duration' in value['text'] and 'Другой лист 🌲' in value['text']
    assert '| 2 |' in value['text'] and '| 3 |' in value['text']
    assert 'сохранённый результат формул' in value['text']
    assert '=SUM' not in value['text'] and path.read_bytes()==before
    assert value['sheets'][0]['missing_formula_cache']==1
    assert value['source_sha256']==hashlib.sha256(before).hexdigest()


def test_sparse_sheet_preserves_actual_row_and_no_fake_zero(tmp_path):
    path=tmp_path/'sparse.xlsx';book=Workbook();sheet=book.active
    sheet['A4']='Номер';sheet['B4']='Описание';sheet['A9']='A9';sheet['B9']='Текст';book.save(path)
    value=read(path)
    assert '| 9 | A9 | Текст |' in value['text']
    assert value['sheets'][0]['header_row']==4


def test_excel_container_signature_wins_over_incorrect_xls_extension(tmp_path):
    path=tmp_path/'export.xlsx';book=Workbook();book.active.append(['Код','Число']);book.active.append(['ЩР-1',25]);book.save(path)
    renamed=tmp_path/'старый экспорт.xls';renamed.write_bytes(path.read_bytes())
    assert read(renamed)['text']==read(path)['text']


def test_structured_reader_keeps_types_headers_and_physical_row_numbers(tmp_path):
    path=tmp_path/'rows.xlsx';book=Workbook();sheet=book.active
    sheet.append(['Преамбула']);sheet.append(['Код','Количество','Согласовано']);sheet.append(['U-1',25,True]);sheet.append([None,None,None]);sheet.append(['U-2',.125,False]);book.save(path)
    value=read(path,view='tables',read_rows=20_000)
    assert value['extractor']=='les.spreadsheet-tables.v1'
    assert value['sheets'][0]['headers']==['Код','Количество','Согласовано']
    assert value['sheets'][0]['rows']==[['U-1',25,True],['U-2',.125,False]]
    assert value['sheets'][0]['row_numbers']==[3,5]
    with pytest.raises(native_core.NativeCoreError,match='SPREADSHEET_LIMIT'):read(path,view='tables',read_rows=2)


def test_projection_is_explicit_bounded_and_does_not_invent_approximate_totals(tmp_path):
    path=tmp_path/'big.xlsx';book=Workbook();sheet=book.active
    sheet.append(['Код','Число']);[sheet.append([f'ЩР-{i}',i+.1]) for i in range(1,101)];book.save(path)
    value=read(path,full_table_max_cells=5,read_rows=20,sample_rows=2)
    assert value['sheets'][0]['projected'] and value['sheets'][0]['truncated']
    assert 'первые 20 строк' in value['text'] and 'Образец строк' in value['text']
    assert 'ЩР-2' in value['text'] and '| 4 |' not in value['text']
    assert 'sum=' not in value['text']


@pytest.mark.parametrize('encoding',['utf-8-sig','utf-16','utf-32'])
def test_csv_multiline_quotes_unicode_and_empty_rows(tmp_path,encoding):
    # Explicit CRLF must not be expanded into CRCRLF by Windows text mode.
    path=tmp_path/'таблица.csv';path.write_text('Код,Описание\r\n"ЩР,1","две\nстроки ""東京"""\r\nB,25\r\n',encoding=encoding,newline='')
    value=read(path)
    assert 'ЩР,1' in value['text'] and 'две<br>строки "東京"' in value['text']
    assert '| 3 | B | 25 |' in value['text']
    assert value['formula_values']=='not_applicable'


@pytest.mark.parametrize('text,code',[
    ('a,b\n"unterminated,x','CSV_QUOTING_INVALID'),('a,b\nx,y,z\n','CSV_ROW_WIDTH_INVALID'),
    ('a,b\nx,\x00\n','DOCUMENT_BINARY_TEXT'),('a,b\n"x"bad,y\n','CSV_QUOTING_INVALID'),
])
def test_csv_damage_never_becomes_document_text(tmp_path,text,code):
    path=tmp_path/'bad.csv';path.write_text(text,encoding='utf-8')
    with pytest.raises(native_core.NativeCoreError,match=code):read(path)


def test_csv_window_reports_more_rows_and_never_reads_partial_quoted_record(tmp_path):
    path=tmp_path/'rows.csv';path.write_text('a,b\n1,"x\ny"\n2,z\n3,q\n',encoding='utf-8')
    value=read(path,read_rows=1)
    assert value['sheets'][0]['truncated'] and 'x<br>y' in value['text']
    assert value['sheets'][0]['last_source_row'] is None
    assert '| 3 |' not in value['text']


def test_empty_and_bad_spreadsheet(tmp_path):
    path=tmp_path/'empty.csv';path.write_text('a,b\n',encoding='utf-8');assert read(path)['text']==''
    path=tmp_path/'broken.xlsx';path.write_bytes(b'broken')
    with pytest.raises(native_core.NativeCoreError,match='DOCUMENT_ARCHIVE_INVALID'):read(path)


def test_slides_use_relationship_order_and_keep_tables_and_entities(tmp_path):
    path=deck(tmp_path/'Доклад.pptx');before=path.read_bytes();value=read(path)
    assert value['slides']==2
    assert value['text'].index('Слайд 10 & 東京 🌲')<value['text'].index('Слайд 2 & 東京 🌲')
    assert 'Строка\nпродолжение' in value['text'] and '| ЩР&#124;1 | 25 кВт |' in value['text']
    assert path.read_bytes()==before


def test_presentation_notes_and_chart_cached_values(tmp_path):
    path=deck(tmp_path/'notes.pptx',order=('r10',),content='<p:sld xmlns:p="p" xmlns:c="c" xmlns:r="r"><c:chart r:id="chart"/></p:sld>')
    with zipfile.ZipFile(path,'a') as z:
        z.writestr('ppt/slides/_rels/slide10.xml.rels','<Relationships><Relationship Id="note" Type="x/notesSlide" Target="../notesSlides/notesSlide1.xml"/><Relationship Id="chart" Type="x/chart" Target="../charts/chart1.xml"/></Relationships>')
        z.writestr('ppt/notesSlides/notesSlide1.xml','<a:notes xmlns:a="a"><a:p><a:t>Заметки автора</a:t></a:p></a:notes>')
        z.writestr('ppt/charts/chart1.xml','<c:chart xmlns:c="c"><c:ser><c:tx><c:v>Мощность</c:v></c:tx><c:cat><c:v>ЭОМ</c:v></c:cat><c:val><c:v>25.5</c:v></c:val></c:ser></c:chart>')
    value=read(path)
    assert 'Заметки автора' in value['text'] and 'Категории: ЭОМ' in value['text'] and 'Значения: 25.5' in value['text']


@pytest.mark.parametrize('kind',['external','duplicate','truncated','dtd'])
def test_presentation_corruption_has_no_partial_success(tmp_path,kind):
    path=deck(tmp_path/'bad.pptx',bad_target=kind=='external',order=('r10','r10') if kind=='duplicate' else ('r10',),
        content='<p:sld xmlns:p="p">' if kind=='truncated' else '<!DOCTYPE doc [<!ENTITY e SYSTEM "file:///secret">]><p:sld xmlns:p="p"/>' if kind=='dtd' else None)
    with pytest.raises(native_core.NativeCoreError,match='PRESENTATION_'):read(path)


def test_decoded_legacy_csv_must_match_source_revision(tmp_path):
    path=tmp_path/'old.csv';path.write_bytes('Код,Имя\n1,Кабель\n'.encode('cp1251'))
    with pytest.raises(native_core.NativeCoreError,match='DOCUMENT_ENCODING_REQUIRED'):read(path)
    with pytest.raises(native_core.NativeCoreError,match='SOURCE_CHANGED'):read(path,decoded_text='other')
    assert 'Кабель' in read(path,decoded_text=path.read_bytes().decode('cp1251'),decoded_sha256=hashlib.sha256(path.read_bytes()).hexdigest())['text']


def test_archive_duplicate_names_and_path_escape_are_rejected(tmp_path):
    path=tmp_path/'unsafe.xlsx'
    with zipfile.ZipFile(path,'w') as z:z.writestr('../outside','x')
    with pytest.raises(native_core.NativeCoreError,match='DOCUMENT_ARCHIVE_INVALID'):read(path)


def worker():
    program=os.getenv('LES_TEST_NATIVE_API')
    if not program:pytest.skip('Native executable required')
    return str(Path(program).resolve())


def test_standalone_worker_runs_without_python_and_survives_document_error(tmp_path):
    program=worker();good=deck(tmp_path/'good.pptx');bad=tmp_path/'bad.xlsx';bad.write_bytes(b'not zip')
    environment=os.environ.copy();environment.update(PATH='',PYTHONHOME=str(tmp_path/'absent'),PYTHONPATH=str(tmp_path/'absent'))
    frames=''.join(json.dumps({'request_id':i,'input':{'schema':'les.document-convert.v1','path':str(path)}})+'\n' for i,path in enumerate((good,bad,good)))
    result=subprocess.run([program,'--serve-document-formats'],input=frames,text=True,encoding='utf-8',capture_output=True,env=environment,timeout=30)
    assert result.returncode==0
    values=[json.loads(line) for line in result.stdout.splitlines()]
    assert [v['request_id'] for v in values]==[0,1,2]
    assert [v['result']['status'] for v in values]==['completed','failed','completed']
    assert values[1]['result']['code']=='DOCUMENT_ARCHIVE_INVALID'
    assert 'document_conversion_failed' in result.stderr


def test_application_converter_uses_rust_not_pandas_or_markitdown(tmp_path,monkeypatch):
    import sys
    from backend import converter
    monkeypatch.setenv('LES_NATIVE_FORMAT_WORKER',worker())
    monkeypatch.setitem(sys.modules,'pandas',None)
    monkeypatch.setattr(converter,'_parse_with_markitdown',lambda *_:pytest.fail('Legacy converter called'))
    path=tmp_path/'data.csv';path.write_text('Код,Описание\n1,Кабель\n',encoding='utf-8')
    assert 'Кабель' in converter.convert_to_markdown(path)
    assert '東京 🌲' in converter.convert_to_markdown(deck(tmp_path/'deck.pptx'))
    path=tmp_path/'broken.xlsx';path.write_bytes(b'broken')
    with pytest.raises(DocumentExtractionError) as error:converter.convert_to_markdown(path)
    assert error.value.code=='DOCUMENT_ARCHIVE_INVALID'
    native_core.required('worker_release_idle')


def test_structured_xls_path_never_converts_through_pandas_or_temporary_xlsx(tmp_path,monkeypatch):
    from backend import parquet_writer
    monkeypatch.setenv('LES_NATIVE_FORMAT_WORKER',worker())
    path=tmp_path/'source.xlsx';book=Workbook();book.active.append(['Код','Количество']);book.active.append(['U-1',25]);book.save(path)
    renamed=tmp_path/'source.xls';renamed.write_bytes(path.read_bytes())
    monkeypatch.setattr(parquet_writer,'_xls_to_xlsx_tmp',lambda *_:pytest.fail('Legacy XLS round trip called'))
    sheets=parquet_writer.read_xlsx_sheets(str(renamed))
    assert sheets[0]['rows']==[{'Код':'U-1','Количество':25}]
    assert sheets[0]['row_numbers']==[2]
    native_core.required('worker_release_idle')


def test_missing_native_worker_has_actionable_installation_error_and_keeps_cause(tmp_path,monkeypatch,caplog):
    from backend.native_document_conversion import read
    monkeypatch.setenv('LES_NATIVE_FORMAT_WORKER',str(tmp_path/'missing.exe'))
    source=tmp_path/'source.csv';source.write_text('a,b\nx,y\n',encoding='utf-8')
    with pytest.raises(DocumentExtractionError) as error:read(source)
    assert error.value.code=='DOCUMENT_CONVERTER_UNAVAILABLE'
    assert str(error.value.__cause__)=='WORKER_PROGRAM_INVALID'
    assert 'WORKER_PROGRAM_INVALID' in caplog.text


def test_worker_is_reused_and_replaced_after_exact_process_kill(tmp_path):
    program=worker();path=deck(tmp_path/'reuse.pptx')
    request={'schema':'les.worker-host.v1','program':program,'directory':str(Path(program).parent),
        'args':['--serve-document-formats'],'context':'format-recovery-test','timeout_ms':10_000,
        'input':{'schema':'les.document-convert.v1','path':str(path)}}
    first=native_core.required('worker_documents',request=request)
    second=native_core.required('worker_documents',request=request)
    assert first['worker_pid']==second['worker_pid'] and second['reused']
    if os.name=='nt':
        import ctypes
        handle=ctypes.windll.kernel32.OpenProcess(1,False,first['worker_pid'])
        assert handle
        try:assert ctypes.windll.kernel32.TerminateProcess(handle,91)
        finally:ctypes.windll.kernel32.CloseHandle(handle)
    else:os.kill(first['worker_pid'],9)
    # The supervisor may observe the dead child before or during the next invoke.
    next_value=native_core.required('worker_documents',request=request)
    if next_value['status']!='completed':next_value=native_core.required('worker_documents',request=request)
    assert next_value['status']=='completed' and next_value['worker_pid']!=first['worker_pid']
    native_core.required('worker_release_idle')
