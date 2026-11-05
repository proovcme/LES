"""Native process ownership and actual format adapter, with no inference services."""
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict
import hashlib
import json
import os
from pathlib import Path
import sys
import subprocess
import time

import psutil
import pytest

from backend import native_core


@pytest.fixture
def native(monkeypatch):
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Build the native core and set LES_CORE_LIBRARY for its integration gate')
    monkeypatch.setenv('LES_CORE_MODE', 'native')
    assert native_core.supports('worker_run')
    native_core.call('worker_stop')
    yield
    native_core.call('worker_stop')


@pytest.fixture
def worker(tmp_path, native):
    script = tmp_path / 'обработчик 日本語.py'
    script.write_text('''import json,os,subprocess,sys,time
from pathlib import Path
for line in sys.stdin:
    envelope=json.loads(line); data=envelope['input']; action=data.get('action')
    if action=='crash':
        print('fixture crash reason',file=sys.stderr,flush=True); os._exit(37)
    if action=='malformed':
        print('invalid-json',flush=True); continue
    if action=='partial':
        sys.stdout.write('{');sys.stdout.flush();os._exit(39)
    if action=='flood':
        sys.stdout.buffer.write(b'x'*(33*1024*1024));sys.stdout.buffer.flush();continue
    if action=='descendant':
        child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)'])
        Path(data['marker']).write_text(str(child.pid))
        time.sleep(120)
    if action=='hang':time.sleep(120)
    print(json.dumps({'request_id':envelope['request_id'],'result':{'pid':os.getpid(),'value':data}},ensure_ascii=False),flush=True)
''', encoding='utf-8')

    def invoke(action='ping', timeout=5000, **data):
        return native_core.call('worker_run', request={
            'schema': 'les.worker-host.v1', 'program': sys.executable,
            'args': ['-B', '-u', str(script)], 'directory': str(tmp_path),
            'context': 'fixture', 'timeout_ms': timeout, 'input': {'action': action, **data}})
    return invoke


@pytest.mark.skipif(sys.platform != 'win32', reason='Abrupt parent exit ownership is a Windows Job contract')
def test_parent_crash_closes_job_and_all_owned_processes(native, tmp_path):
    worker_script=tmp_path/'worker.py'
    worker_script.write_text('''import json,os,subprocess,sys,time
for line in sys.stdin:
    envelope=json.loads(line)
    child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)'])
    print(json.dumps({'request_id':envelope['request_id'],'result':{'child_pid':child.pid}}),flush=True)
''',encoding='utf-8')
    request={'schema':'les.worker-host.v1','program':sys.executable,
             'args':['-u',str(worker_script)],'directory':str(tmp_path),
             'context':'parent-exit','timeout_ms':5000,'input':{}}
    request_path=tmp_path/'request.json'; request_path.write_text(json.dumps(request),encoding='utf-8')
    result_path=tmp_path/'result.json'
    parent=subprocess.run([sys.executable,'-c',
        'import json,os,sys;from pathlib import Path;from backend import native_core;'
        'result=native_core.call("worker_run",request=json.loads(Path(sys.argv[1]).read_text()));'
        'Path(sys.argv[2]).write_text(json.dumps(result));os._exit(42)',
        str(request_path),str(result_path)],cwd=Path(__file__).resolve().parents[1],timeout=10)
    assert parent.returncode == 42
    result=json.loads(result_path.read_text())
    assert result['status'] == 'completed'
    assert_dead(result['worker_pid'])
    assert_dead(result['result']['child_pid'])


def assert_dead(pid):
    deadline = time.monotonic() + 3
    while psutil.pid_exists(pid) and time.monotonic() < deadline:
        try:
            if psutil.Process(pid).status() == psutil.STATUS_ZOMBIE:
                return
        except psutil.NoSuchProcess:
            return
        time.sleep(.02)
    assert not psutil.pid_exists(pid), f'Owned fixture process {pid} survived'


def test_warm_reuse_and_explicit_stop(worker):
    first = worker(text='Русский 日本語 — пробелы')
    second = worker(text='следующий')
    assert first['status'] == second['status'] == 'completed'
    assert first['worker_pid'] == second['worker_pid']
    assert first['reused'] is False and second['reused'] is True
    assert first['result']['value']['text'] == 'Русский 日本語 — пробелы'
    native_core.call('worker_stop')
    assert_dead(first['worker_pid'])
    assert worker()['worker_pid'] != first['worker_pid']


def test_api_shutdown_closes_worker_before_interpreter_thread_join(worker):
    from proxy.app import create_app
    from backend.format_process import stop
    application=create_app()
    result=worker()
    assert stop in application.router.on_shutdown
    stop()
    assert_dead(result['worker_pid'])


@pytest.mark.parametrize(('action', 'code'), [
    ('crash', 'WORKER_EXITED'), ('malformed', 'WORKER_RESPONSE_INVALID'),
    ('partial', 'WORKER_RESPONSE_INCOMPLETE'), ('flood', 'WORKER_OUTPUT_LIMIT'),
    ('hang', 'WORKER_TIMEOUT'),
])
def test_failure_is_bounded_and_next_file_starts_clean(worker, action, code):
    started = time.monotonic()
    result = worker(action, timeout=1500 if action == 'hang' else 5000)
    assert result['status'] == 'failed' and result['code'] == code
    assert time.monotonic() - started < 8
    assert_dead(result['worker_pid'])
    if action == 'crash':
        assert result['exit_code'] == 37
        assert 'fixture crash reason' in result['diagnostics']
    next_result = worker()
    assert next_result['status'] == 'completed'
    assert not next_result['reused']
    assert next_result['worker_pid'] != result['worker_pid']


def test_stop_cancels_active_conversion_and_its_descendant(worker, tmp_path):
    marker = tmp_path / 'child.pid'
    with ThreadPoolExecutor(max_workers=1) as pool:
        future = pool.submit(worker, 'descendant', timeout=30000, marker=str(marker))
        deadline = time.monotonic() + 5
        while not marker.exists() and time.monotonic() < deadline:
            time.sleep(.02)
        assert marker.exists()
        child_pid = int(marker.read_text())
        started = time.monotonic()
        native_core.call('worker_stop')
        result = future.result(timeout=3)
    assert time.monotonic() - started < 3
    assert result['code'] == 'WORKER_CANCELLED'
    assert_dead(child_pid)
    assert_dead(result['worker_pid'])
    assert worker()['status'] == 'completed'


def test_native_inventory_unicode_exclusions_and_fail_closed(native, tmp_path):
    from backend.external_scan import source_files
    (tmp_path / '_originals').mkdir()
    (tmp_path / '_originals' / 'ignored.pdf').write_bytes(b'not read')
    (tmp_path / 'Раздел 日本語').mkdir()
    expected = [tmp_path / 'Раздел 日本語' / 'Щит  2.txt', tmp_path / 'é.txt']
    for path in expected:
        path.write_bytes(b'fixture')
    assert set(source_files(tmp_path)) == set(expected)
    with pytest.raises(ValueError, match='слишком велика'):
        list(source_files(tmp_path, limit=1))
    with pytest.raises(OSError):
        list(source_files(tmp_path / 'missing'))


def test_worker_preserves_numeric_metadata_exactly(worker):
    # Real filesystem/email timestamps occupy these finite binary64 values.
    # Crossing Python -> Rust -> worker -> Rust must not round them again.
    values = [1793535000.0 + index / 10_000_000 for index in range(1000)]
    result = worker('echo', values=values)
    assert result['result']['value']['values'] == values


def test_worker_error_keeps_stable_retry_code_and_human_message():
    from backend.format_process import FormatProcessError
    from backend.qdrant_support import _classify_parse_failure
    error=FormatProcessError('WORKER_TIMEOUT')
    disposition=_classify_parse_failure(error, attempts=1)
    assert disposition.error_code == 'WORKER_TIMEOUT' and disposition.retryable
    assert 'слишком много времени' in str(error)
    assert not _classify_parse_failure(error, attempts=100).retryable
    assert not _classify_parse_failure(FormatProcessError('WORKER_OUTPUT_LIMIT'), attempts=1).retryable


@pytest.fixture
def formats(tmp_path):
    from email.message import EmailMessage
    from docx import Document
    from openpyxl import Workbook
    from tests.pdf_fixtures import write_pdf
    root = tmp_path / 'Исходники 日本語'
    root.mkdir()
    (root / 'Текст  1.txt').write_text('Контрольный кабель PE. Сечение 25 мм2.\n' * 3, encoding='utf-8')
    (root / 'Кодировка.txt').write_bytes(('Планируем открыть библиотеку. Ответственная Марина Лесная. '
                                       'Срок завершения — июнь 2033 года. ' * 4).encode('cp1251'))
    (root / 'Таблица.csv').write_text('Наименование;Количество;Единица\nКабель;70;м\n', encoding='utf-8-sig')
    book = Workbook(); sheet = book.active; sheet.title = 'Щиты'
    sheet.append(['Наименование', 'Количество', 'Единица']); sheet.append(['Кабель', 70, 'м'])
    book.save(root / 'Таблица.xlsx'); book.close()
    doc = Document(); doc.add_paragraph('Контрольный раздел. Кабель PE сечением 25 мм2.')
    doc.save(root / 'Раздел.docx')
    write_pdf(root / 'Чертёж.pdf', ['Evidence fixture: cable PE 25 mm2.'])
    mail=EmailMessage(); mail['Subject']='Контрольный раздел'; mail['From']='author@example.invalid'
    mail['To']='reader@example.invalid'; mail['Message-ID']='<format-fixture@example.invalid>'
    mail.set_content('Требуется проверить контрольный кабель PE. Сечение 25 мм2. ' * 3)
    (root / 'Письмо.eml').write_bytes(mail.as_bytes())
    return sorted(root.iterdir())


def test_real_formats_match_inline_and_sources_remain_unchanged(native, formats, tmp_path, monkeypatch):
    from backend import format_process, qdrant_support as support
    from backend.qdrant_ingestion import QdrantIngestion
    from backend.qdrant_nodes import QdrantNodes
    monkeypatch.setenv('LES_FORMAT_WORKER_MODE', 'native')
    monkeypatch.setenv('RAG_CONVERT_SUBPROCESS_ENABLED', 'false')
    monkeypatch.setenv('PDF_TABLE_EXTRACTION_ENABLED', 'false')
    monkeypatch.setenv('RAG_CHUNK_UNIT', 'chars')
    monkeypatch.setenv('LES_MAIL_REGISTRY_DB', str(tmp_path / 'mail-registry.db'))
    splitter = support.StructureAwareSplitter(chunk_size=1000, chunk_overlap=100)
    adapter = QdrantNodes()
    hashes = {path: hashlib.sha256(path.read_bytes()).hexdigest() for path in formats}
    for path in formats:
        baseline_route, baseline_nodes = QdrantIngestion._convert_file_inline(
            adapter, path, tmp_path, path.name, 'fixture', support.MarkdownNodeParser(), splitter, {}, False)
        route, nodes = QdrantIngestion._convert_file(
            adapter, path, tmp_path, path.name, 'fixture', support.MarkdownNodeParser(), splitter, {}, False)
        assert asdict(route) == asdict(baseline_route), path.suffix
        # MarkdownNodeParser assigns fresh UUIDs in both processes. Compare all
        # evidence text and provenance, rather than those incidental UUIDs.
        actual = [{k: v for k, v in node.items() if k != 'doc_id'} for node in nodes]
        expected = [{k: v for k, v in node.items() if k != 'doc_id'} for node in baseline_nodes]
        differences = [{key: [current.get('payload', {}).get(key), previous.get('payload', {}).get(key)]
                        for key in current.get('payload', {}).keys() | previous.get('payload', {}).keys()
                        if current.get('payload', {}).get(key) != previous.get('payload', {}).get(key)}
                       for current, previous in zip(actual, expected)]
        assert actual == expected, (path.suffix, differences)
        assert nodes and any(node['text'].strip() for node in nodes), path.suffix
    assert {path: hashlib.sha256(path.read_bytes()).hexdigest() for path in formats} == hashes
