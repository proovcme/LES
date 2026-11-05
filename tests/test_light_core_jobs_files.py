"""One SQLite authority, restart safety and byte-identical fingerprints in both engines."""
from concurrent.futures import ThreadPoolExecutor
import hashlib
import os
import sqlite3
from threading import Barrier
import pytest

from backend import native_core
from backend.file_fingerprint import fingerprint
from backend.job_policy import recovery_patch
from proxy.services.job_service import JobService


@pytest.fixture(params=['python', 'native'])
def engine(request, monkeypatch):
    if request.param == 'native' and not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Set LES_CORE_LIBRARY to run the native integration gate')
    monkeypatch.setenv('LES_CORE_MODE', request.param)
    if request.param == 'python':
        from tests.oracles.job_store import JobService as Oracle
        monkeypatch.setitem(globals(), 'JobService', Oracle)
    return request.param


def test_readonly_fingerprint_unicode_empty_and_same_metadata_changes(tmp_path, engine):
    path = tmp_path/'Папка с пробелами/Текст.txt'; path.parent.mkdir()
    for content in (b'', 'PE РЕ 😀'.encode('utf-8'), b'a'*3_000_001):
        path.write_bytes(content)
        before = path.stat()
        value = fingerprint(path)
        assert value == {'sha256':hashlib.sha256(content).hexdigest(), 'size':len(content), 'stable':True}
        assert path.read_bytes() == content and path.stat().st_mtime_ns == before.st_mtime_ns
    previous = fingerprint(path)
    path.write_bytes(b'b'*3_000_001); os.utime(path, ns=(before.st_atime_ns,before.st_mtime_ns))
    assert fingerprint(path)['sha256'] != previous['sha256']
    with pytest.raises(OSError): fingerprint(tmp_path/'missing')
    with pytest.raises(OSError): fingerprint(path.parent)


def test_python_fingerprint_rejects_change_during_read(tmp_path, monkeypatch):
    monkeypatch.setenv('LES_CORE_MODE','python')
    path = tmp_path/'source'; path.write_bytes(b'original')
    original = hashlib.file_digest
    def replace(stream, algorithm):
        digest = original(stream,algorithm)
        path.write_bytes(b'changed size')
        return digest
    monkeypatch.setattr(hashlib,'file_digest',replace)
    with pytest.raises(OSError, match='изменился'): fingerprint(path)


def test_jobs_resume_counters_and_recover_only_active_once(tmp_path, engine):
    path = tmp_path/'jobs.sqlite'
    store = JobService(path)
    pending = store.create('test', total=3)
    store.update(pending['id'], status='parsing', processed=2, message='Текст готов')
    complete = store.create('test', status='running', total=1)
    store.update(complete['id'], status='completed', processed=1, result={'готово':True})
    final = store.get(complete['id'])
    # A delayed progress callback cannot resurrect a finished job or overwrite its counters.
    store.update(complete['id'], status='running', processed=0)
    store.update(complete['id'], processed=0)
    assert store.get(complete['id']) == final
    restarted = JobService(path)
    assert restarted.get(pending['id'])['processed'] == 2
    assert restarted.mark_interrupted_active_jobs('restart') == 1
    interrupted = restarted.get(pending['id'])
    assert interrupted['status']=='cancelled' and interrupted['processed']==2
    assert interrupted['message']=='Текст готов | Interrupted by restart'
    assert restarted.mark_interrupted_active_jobs('again')==0
    assert restarted.get(complete['id'])['result']=={'готово':True}


@pytest.mark.parametrize('updates',[{'processed':-1}, {'total':True}, {'errors':2**63},
    {'status':'bogus'}, {'message':None}, {'result':'not JSON'}, {"status='failed' --":1}])
def test_invalid_job_patches_do_not_modify_sqlite(tmp_path, engine, updates):
    store = JobService(tmp_path/'jobs.sqlite'); job=store.create('test')
    with pytest.raises((ValueError,native_core.NativeCoreError)):
        store.update(job['id'],**updates)
    assert store.get(job['id'])==job


def test_failed_recovery_rolls_back_every_row_and_connections_close(tmp_path, engine, monkeypatch):
    store = JobService(tmp_path/'jobs.sqlite')
    ids=[store.create('test')['id'] for _ in range(3)]
    if engine == 'python':
        import tests.oracles.job_store as module
        original=module.recovery_patch;calls=[]
        def fail(current,reason,now):
            calls.append(current)
            if len(calls)==2:raise RuntimeError('synthetic crash')
            return original(current,reason,now)
        monkeypatch.setattr(module,'recovery_patch',fail)
        with pytest.raises(RuntimeError,match='synthetic crash'):store.mark_interrupted_active_jobs('restart')
    else:
        with sqlite3.connect(store.db_path) as conn:
            conn.execute("CREATE TRIGGER fail_second BEFORE UPDATE ON jobs WHEN OLD.id='"+ids[1]+"' BEGIN SELECT RAISE(ABORT,'synthetic crash'); END")
        with pytest.raises(sqlite3.OperationalError):store.mark_interrupted_active_jobs('restart')
        with sqlite3.connect(store.db_path) as conn:conn.execute('DROP TRIGGER fail_second')
    assert all(store.get(job)['status']=='queued' for job in ids)
    # No leaked transaction: another connection can immediately take the writer lease.
    with sqlite3.connect(store.db_path, timeout=.1) as conn:
        conn.execute('BEGIN EXCLUSIVE')
        conn.rollback()


def test_sqlite_serializes_competing_progress_and_completion(tmp_path, engine):
    store=JobService(tmp_path/'jobs.sqlite');job=store.create('test',status='running',total=10)
    def update(i):
        store.update(job['id'],status='completed' if i==3 else 'running',processed=i)
    with ThreadPoolExecutor(max_workers=4) as pool:list(pool.map(update,range(10)))
    final=store.get(job['id'])
    assert final['status']=='completed' and final['processed']==3 and final['finished_at']


def test_terminal_job_recovery_is_noop():
    # Cross-engine parity is covered by the parameterized SQLite scenarios;
    # transition policy is now mandatory native code, including terminal no-ops.
    assert recovery_patch({'status':'completed'},'restart','now')['ignored']


def test_competing_manual_and_auto_claims_have_one_durable_owner(tmp_path):
    path = tmp_path / 'jobs.sqlite'
    stores = [JobService(path) for _ in range(8)]
    ready = Barrier(len(stores))

    def claim(index):
        ready.wait(timeout=10)
        return stores[index].create(
            'rag_parse_scheduler', source='auto_resume' if index % 2 else 'pending',
            status='running', total=7,
        )

    with ThreadPoolExecutor(max_workers=len(stores)) as pool:
        claims = [row for row in pool.map(claim, range(len(stores))) if row is not None]
    assert len(claims) == 1
    store = stores[0]
    winner = claims[0]['id']
    assert list(store.list()) == [winner]
    store.update(winner, processed=3)
    assert store.mark_interrupted_active_jobs('restart') == 1
    assert store.get(winner)['processed'] == 3
    assert store.create('rag_parse_scheduler', status='running', total=4) is not None

def test_job_result_large_integer_unicode_and_float_roundtrip(tmp_path, engine):
    store=JobService(tmp_path/'jobs.sqlite');job=store.create('exact metadata')
    payload={'number':2**80+123,'time':1793563000.1234567,'name':'日本語 🌲','path':r'C:\folder with spaces\file.txt'}
    store.update(job['id'],result=payload)
    assert store.get(job['id'])['result']==payload
    assert store.list()[job['id']]['result']==payload
    with sqlite3.connect(store.db_path)as db:
        import json
        assert json.loads(db.execute('SELECT result FROM jobs WHERE id=?',(job['id'],)).fetchone()[0])==payload
