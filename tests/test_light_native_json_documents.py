"""JSON source failures cannot become searchable document content."""
import hashlib
import json
import os
from pathlib import Path

import pytest

from backend import native_core
from backend.converter import convert_to_markdown
from backend.extraction_errors import DocumentExtractionError
from backend.json_document import read


@pytest.fixture(autouse=True)
def native(monkeypatch):
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Native JSON reader required')
    assert native_core.supports('document_json')
    monkeypatch.setenv('RAG_MAX_FILE_CHARS','500000')


def write(root, value, suffix='.json', encoding='utf-8'):
    path = root/('Источник 日本語 # 1'+suffix)
    path.write_bytes(value.encode(encoding))
    return path


def test_generic_keys_numbers_arrays_and_nested_text_are_preserved(tmp_path):
    value={'роль':'проект','данные':{'мощность':123.456,'единица':'кВт','ноль':0,'готов':False},
           'список':[{'позиция':1,'quantity':7},'日本語 中文 العربية 😀'],
           'text':'Полный текст '+'продолжение '*300}
    path=write(tmp_path,json.dumps(value,ensure_ascii=False));before=path.read_bytes()
    text=convert_to_markdown(path)
    for expected in ['123.456','кВт','**ноль:** 0','false','quantity','日本語 中文 العربية 😀',value['text']]:
        assert expected in text
    assert path.read_bytes()==before and list(tmp_path.iterdir())==[path]


@pytest.mark.parametrize('content', ['{"text":','{"text":"Данные",}','[1,2,]','{"x": undefined}'])
def test_invalid_json_raises_a_readable_error_instead_of_returning_error_text(tmp_path,content):
    path=write(tmp_path,content);before=path.read_bytes()
    with pytest.raises(DocumentExtractionError) as fault:
        convert_to_markdown(path)
    assert fault.value.code=='JSON_INVALID'
    assert 'Строка' in str(fault.value) and 'позиция' in str(fault.value)
    assert '[ERROR]' not in str(fault.value)
    assert path.read_bytes()==before and list(tmp_path.iterdir())==[path]


@pytest.mark.parametrize('content',['','  ','[]','{}'])
def test_empty_documents_never_produce_a_warning_chunk(tmp_path,content):
    path=write(tmp_path,content)
    with pytest.raises(DocumentExtractionError) as fault:
        convert_to_markdown(path)
    assert fault.value.code=='NO_SEARCHABLE_TEXT'


def test_error_words_in_real_source_data_are_not_filtered(tmp_path):
    value='[ERROR] This is actual source data, not an exception from the parser.'
    assert value in convert_to_markdown(write(tmp_path,json.dumps({'text':value})))


def test_jsonl_and_auto_detection_preserve_order_and_do_not_skip_bad_lines(tmp_path):
    path=write(tmp_path,'\n{"text":"Первое сообщение"}\n\n{"text":"Второе сообщение"}\n')
    text=convert_to_markdown(path)
    assert text.index('Первое сообщение')<text.index('Второе сообщение')
    path.write_text('{"text":"Первое сообщение"}\n{broken}\n{"text":"Второе сообщение"}',encoding='utf-8')
    path=path.rename(path.with_suffix('.jsonl'))
    with pytest.raises(DocumentExtractionError) as fault:
        convert_to_markdown(path)
    assert fault.value.code=='JSON_INVALID' and 'Строка 2' in str(fault.value)


def test_record_and_character_limits_are_visible_in_the_indexed_text(tmp_path):
    path=write(tmp_path,json.dumps([{'text':f'Содержимое записи номер {i:04d}'} for i in range(2003)],ensure_ascii=False))
    text=convert_to_markdown(path)
    assert 'Документ показан частично' in text
    assert 'номер 1999' in text and 'номер 2000' not in text
    path.write_text(json.dumps({'text':'Большой текст 日本語 😀 '*1000},ensure_ascii=False),encoding='utf-8')
    text=read(path,max_chars=300)
    assert len(text)<=300 and 'Документ показан частично' in text and 'Большой текст' in text


def test_python_legacy_nonfinite_numbers_remain_literal_without_changing_strings(tmp_path):
    path=write(tmp_path,'{"number":NaN,"large":Infinity,"negative":-Infinity,"text":"NaN Infinity are literal words"}')
    text=convert_to_markdown(path)
    assert 'Нестандартные числовые значения показаны буквально' in text
    assert '**number:** NaN' in text and '**negative:** -Infinity' in text
    assert 'NaN Infinity are literal words' in text


def test_large_utf8_input_bypasses_the_python_encoding_decoder(tmp_path,monkeypatch):
    from backend import text_decoding
    def unexpected(*args):
        pytest.fail('UTF-8 JSON must be decoded by Rust')
    monkeypatch.setattr(text_decoding,'read_document_text',unexpected)
    path=write(tmp_path,json.dumps({'text':'Важное содержимое проекта','bulk':'X'*(9*1024*1024)},ensure_ascii=False))
    text=read(path,max_chars=400)
    assert len(text)<=400 and 'Важное содержимое проекта' in text
    assert 'Документ показан частично' in text


def test_nested_empty_values_remain_distinguishable(tmp_path):
    text=convert_to_markdown(write(tmp_path,'{"body":[],"description":{},"text":""}'))
    assert '**body:** []' in text and '**description:** {}' in text
    assert '(пустая строка)' in text


def test_large_unicode_output_crosses_the_bridge_without_splitting_characters(tmp_path,monkeypatch):
    value='Данные 日本語 🌲 с пробелами. '*60000
    path=write(tmp_path,json.dumps({'text':value},ensure_ascii=False))
    original=native_core.run_json_document;parts=[]
    def observe(request,bridge):
        def exchange(operation,value):
            if operation==0:parts.append(value['text'])
            return bridge(operation,value)
        return original(request,exchange)
    monkeypatch.setattr(native_core,'run_json_document',observe)
    text=read(path,max_chars=2_000_000)
    assert value in text and len(parts)>1
    assert all(part.encode('utf-8') for part in parts)
    assert list(tmp_path.iterdir())==[path]


def test_large_compatibility_decode_is_passed_in_bounded_memory_chunks(tmp_path,monkeypatch):
    from backend import text_decoding
    # Transport fixture: actual CP1251 detection is tested separately above.
    value='Содержимое 日本語 🌲. '*50000
    document=json.dumps({'text':value},ensure_ascii=False)
    path=tmp_path/'Legacy.json';path.write_bytes(b'\xff\xfe\x01')
    calls=[]
    def decode(source):
        calls.append(source)
        return document
    monkeypatch.setattr(text_decoding,'read_document_text',decode)
    assert value in read(path,max_chars=2_000_000)
    assert calls==[path] and path.read_bytes()==b'\xff\xfe\x01'


@pytest.mark.parametrize('encoding',['utf-8-sig','utf-16','utf-32','cp1251'])
def test_encodings_and_original_bytes_are_preserved(tmp_path,encoding):
    value='Лес помнит текст, содержит данные проекта и сохраняет все буквы. '*8
    path=write(tmp_path,json.dumps({'text':value},ensure_ascii=False),encoding=encoding)
    before=path.read_bytes()
    assert value in convert_to_markdown(path)
    assert path.read_bytes()==before and list(tmp_path.iterdir())==[path]


def test_output_can_never_overwrite_the_source(tmp_path):
    path=write(tmp_path,'{"text":"Настоящее содержимое документа"}')
    before=hashlib.sha256(path.read_bytes()).hexdigest()
    with pytest.raises(native_core.NativeCoreError,match='JSON_READ_ONLY'):
        native_core.required('document',request={'action':'json_document','path':str(path),'output':str(path),'max_chars':1000})
    assert hashlib.sha256(path.read_bytes()).hexdigest()==before


def test_malformed_json_is_not_published_or_embedded_by_the_native_batch(tmp_path):
    from tests.test_light_native_index_batch import make_case, Boundary, rows
    request=make_case(tmp_path,('Bad.json',))
    path=Path(request['content_root'])/'ds/Bad.json'
    path.write_text('{"text": broken}',encoding='utf-8')
    def convert(value):
        convert_to_markdown(Path(value['path']))
        pytest.fail('A malformed source cannot return document text')
    boundary=Boundary(request,convert=convert)
    result=native_core.run_index_batch(request,boundary)
    assert result['errors']==1 and boundary.saved==[]
    assert not any(operation==1 for operation,_ in boundary.events)
    row,=rows(request)
    assert row['status']=='ERROR' and row['chunk_count']==0 and row['error_code']=='JSON_INVALID'
    assert '[ERROR]' not in row['last_error'] and 'Строка' in row['last_error']


def test_scan_without_extracted_table_text_never_becomes_a_warning_node(tmp_path,monkeypatch):
    from backend import qdrant_support as support
    from backend.qdrant_nodes import QdrantNodes
    class Normalizer:
        def __init__(self,**kwargs):pass
        async def process(self,*args,**kwargs):
            return {'chunks':[],'needs_ocr':True,'scanned_pages':[1,2]}
    monkeypatch.setattr(support,'TableNormalizer',Normalizer)
    with pytest.raises(DocumentExtractionError) as fault:
        QdrantNodes()._sync_table_nodes(tmp_path/'Scan.pdf',tmp_path/'derived','Scan.pdf','dataset')
    assert fault.value.code=='NO_SEARCHABLE_TEXT'
