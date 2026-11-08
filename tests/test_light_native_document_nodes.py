"""Full node equality with retained preparation, including existing IDs and page context."""
from copy import deepcopy
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time

import pytest

from backend import native_core, qdrant_support as support
from backend.qdrant_nodes import QdrantNodes


@pytest.fixture(autouse=True)
def native(monkeypatch):
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Native node preparation ABI required')
    assert native_core.supports('document_nodes')
    monkeypatch.setattr(support, 'FINAL_MIN_CHUNK', 20)
    monkeypatch.setattr(support, 'current_dataset_revision_id', lambda _: 'révision-東京')
    monkeypatch.setenv('RAG_PARENT_WINDOW_CHUNKS', '3')


def compare(nodes, file='Раздел/Проект 日本語.pdf', *, size=120, tokenizer=None, minimum=20):
    monkey_minimum = support.FINAL_MIN_CHUNK
    assert monkey_minimum == minimum
    expected = QdrantNodes._finalize_embedding_nodes(deepcopy(nodes), chunking={
        'unit': 'tokens' if tokenizer else 'chars', 'chunk_size': size, 'len_fn': tokenizer})
    support._apply_context_metadata_to_nodes(expected, 'dataset_日本語', file)
    raw = deepcopy(nodes)
    for node in raw:
        payload = node.get('payload') or {}
        node['_les_file_typing'] = support.chunk_payload_typing(file, payload, payload)
    value = native_core.required('document', request={'action': 'prepare_nodes', 'nodes': raw,
        'dataset_id': 'dataset_日本語', 'file_name': file, 'revision': 'révision-東京', 'window': 3,
        'config': {'unit': 'tokens' if tokenizer else 'chars', 'size': size, 'minimum': minimum,
                   'tokenizer': tokenizer.asset if tokenizer else None}})['nodes']
    assert value == expected
    assert all('_les_file_typing' not in node for node in value)
    return value


def node(text, **payload):
    return {'text': text, 'doc_id': 'raw:' + str(payload.get('source_page', 'body')),
            'payload': payload, 'extra': {'preserve': ['東京', '😀']}}


def test_headings_inheritance_navigation_and_neighbor_windows():
    value = compare([
        node('# Общий раздел\n' + 'Проверка продолжения текста 😀. ' * 12),
        node('## Дочерний раздел\n' + 'Текст с единицами м² и кодами ЩР-1. ' * 7),
        node('2.3.1 Точный пункт\n' + 'Требование с несколькими условиями. ' * 9),
        node('1.2 lower case is ordinary text. ' * 3),
        node('# Общий раздел\n' + 'Повторный заголовок не создаёт дубликат узла. ' * 3),
    ], file='Проект.md')
    assert any(v['payload'].get('heading_inherited') for v in value)
    assert any(v['payload']['node_role'] == 'navigation' for v in value)


def test_page_edges_tables_sheet_names_and_zero_ordinals():
    compare([
        node('1.1 Первый пункт\n' + 'Содержимое первого листа. ' * 8, source_page=1, table_index=0),
        node('Строка таблицы | Кабель | 25 м | Примечание. ' * 5,
             source_page=1, table_index=0, type='table_row', source_sheet='Лист 1'),
        node('Продолжение строки | Кабель | 30 м | Примечание. ' * 5,
             source_page=2, table_index=0, type='table_row', source_sheet='Лист 2'),
        node('Нулевая страница не должна менять существующий порядок. ' * 3, page_number=0),
    ], file='Папка/Ведомость.xlsx')


def test_existing_payload_defaults_and_cross_page_custom_parent():
    compare([
        node('Существующие поля не перезаписываются при раскрытии. ' * 3,
             source_page=1, parent_id='custom', parent_ord=9, child_ord=12,
             section_heading='Уже сохранённый заголовок', context_after='prepared next'),
        node('Сосед сохраняет свою идентичность и предыдущее окружение. ' * 3,
             source_page=2, parent_id='custom', content_hash='existing-hash', context_before='prepared previous'),
    ])


@pytest.mark.parametrize('separator', ['\r\n', '\u0085', '\u2028', '\u2029'])
def test_unicode_line_boundaries_and_whitespace(separator):
    compare([node(separator.join(['', '\u00a0', '## Данные 日本語', 'Текст 😀 с пробелами\u00a0и единицами. ' * 8]))])


def test_empty_short_and_sanitized_nodes_preserve_filtering():
    compare([node(''), node('Коротко'), node('## Заголовок\n' + 'данные\x00\x01\t  ' * 30)])


def test_selected_local_tokenizer_preserves_exact_budget_and_hashes(tmp_path):
    import json
    asset = tmp_path/'tokenizer.json'
    asset.write_text(json.dumps({'version':'1.0','truncation':None,'padding':None,'added_tokens':[],
        'normalizer':None,'pre_tokenizer':{'type':'Whitespace'},'post_processor':None,'decoder':None,
        'model':{'type':'WordLevel','vocab':{'[UNK]':0,'word':1,'данные':2,'.':3},'unk_token':'[UNK]'}},
        ensure_ascii=False),encoding='utf-8')
    native_tokenizer = native_core.LocalTokenizer(asset)
    value = compare([node('## Раздел\n' + 'word данные 😀. ' * 30)], size=24, tokenizer=native_tokenizer)
    evidence = [v for v in value if v['payload']['node_role'] == 'evidence']
    assert len(evidence)>1
    assert all(v['payload']['embedding_chunk_length']<=24 for v in evidence)


def batch_request(root, names=('Документ.txt',)):
    from tests.test_light_native_index_batch import make_case
    value = make_case(root, names)
    value['node_preparation'] = {'unit':'chars','size':256,'minimum':20,'window':4,'tokenizer':None}
    return value


def raw_boundary(request, *, pause_second_embedding=False, extra_row=False, bad_typing=False):
    from tests.test_light_native_index_batch import Boundary
    owner = Boundary(request)
    embedding_calls = 0
    def exchange(operation, value):
        nonlocal embedding_calls
        if operation == 1:
            embedding_calls += 1
            if pause_second_embedding and embedding_calls == 2:
                (Path(request['database']).parent/'paused').write_text('ready')
                time.sleep(60)
        result = owner(operation, value)
        if operation == 0:
            result['node_state'] = 'les.nodes.raw.v1'
            if bad_typing:
                path = Path(value['node_spool'])
                node = json.loads(path.read_text(encoding='utf-8'))
                node['_les_file_typing'] = {'unrecognized_field': 'invalid annotation'}
                path.write_text(json.dumps(node)+'\n',encoding='utf-8')
            if extra_row:
                path = Path(value['node_spool'])
                with path.open('ab') as stream:
                    stream.write(path.read_bytes())
        return result
    return owner, exchange


def test_raw_batch_publishes_only_native_prepared_nodes_and_cleans_both_spools(tmp_path):
    request = batch_request(tmp_path)
    owner, exchange = raw_boundary(request)
    result = native_core.run_index_batch(request, exchange)
    assert result['errors'] == 0 and result['files_parsed'] == 1
    assert owner.saved and all(value['payload']['embedding_budget_enforced'] for value in owner.saved)
    assert all(value['payload']['node_role'] == 'evidence' and value['payload']['parent_id'] for value in owner.saved)
    assert not list(Path(request['spool_directory']).iterdir())


def test_native_preparation_records_a_readable_cause_without_publishing(tmp_path):
    from tests.test_light_native_index_batch import rows
    from backend.extraction_errors import MESSAGES
    request = batch_request(tmp_path)
    owner, exchange = raw_boundary(request,bad_typing=True)
    result = native_core.run_index_batch(request,exchange)
    assert result['errors'] == 1 and owner.saved == []
    row, = rows(request)
    assert row['error_code'] == 'DOCUMENT_NODES_INVALID'
    assert row['last_error'] == MESSAGES['DOCUMENT_NODES_INVALID']


def test_raw_spool_with_an_unreported_extra_row_never_embeds_or_publishes(tmp_path):
    request = batch_request(tmp_path)
    owner, exchange = raw_boundary(request, extra_row=True)
    result = native_core.run_index_batch(request, exchange)
    assert result['errors'] == 1 and owner.saved == []
    assert not any(operation == 1 for operation, _ in owner.events)
    assert not list(Path(request['spool_directory']).iterdir())


@pytest.mark.parametrize('changes,code', [
    ({'size':0}, 'DOCUMENT_NODES_INVALID'),
    ({'unit':'tokens','tokenizer':None}, 'DOCUMENT_TOKENIZER_UNAVAILABLE'),
])
def test_invalid_preparation_is_rejected_before_claiming_a_document(tmp_path,changes,code):
    from tests.test_light_native_index_batch import rows
    request = batch_request(tmp_path)
    request['node_preparation'].update(changes)
    owner, exchange = raw_boundary(request)
    with pytest.raises(native_core.NativeCoreError,match=code):
        native_core.run_index_batch(request,exchange)
    assert owner.events == [] and rows(request)[0]['parse_attempts'] == 0


def crash_raw(request):
    _, exchange = raw_boundary(request, pause_second_embedding=True)
    native_core.run_index_batch(request, exchange)


def test_native_preparation_survives_owner_death_without_repeating_committed_file(tmp_path):
    from tests.test_light_native_index_batch import rows
    request = batch_request(tmp_path, ('A.txt','B.txt'))
    file = tmp_path/'request.json'
    file.write_text(json.dumps(request), encoding='utf-8')
    script = ('import json,sys;sys.path[:0]=sys.argv[1:3];'
              'from tests.test_light_native_document_nodes import crash_raw;'
              'crash_raw(json.load(open(sys.argv[3],encoding="utf-8")))')
    root = Path(__file__).resolve().parents[1]
    with (tmp_path/'child.log').open('wb') as log:
        child = subprocess.Popen([getattr(sys,'_base_executable',sys.executable),'-I','-S','-c',script,
            str(root),str(root/'.venv/Lib/site-packages'),str(file)], stdout=log,stderr=log,
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        try:
            deadline = time.monotonic()+15
            marker = Path(request['database']).parent/'paused'
            while not marker.exists() and time.monotonic()<deadline:
                assert child.poll() is None
                time.sleep(.03)
            assert marker.exists()
            assert list(Path(request['spool_directory']).glob('*.prepared.nodes'))
            child.kill();child.wait(5)
        finally:
            if child.poll() is None:
                child.kill();child.wait(5)
    owner, exchange = raw_boundary(request)
    result = native_core.run_index_batch(request,exchange)
    assert result['files_parsed'] == 1 and result['errors'] == 0
    assert len(owner.saved) == 1
    assert [value['parse_attempts'] for value in rows(request)] == [1,2]
    assert not list(Path(request['spool_directory']).iterdir())
    with sqlite3.connect(request['database']) as conn:
        assert conn.execute("SELECT stop_code FROM les_index_batches WHERE status='interrupted'").fetchone() == ('INDEX_BATCH_OWNER_LOST',)
