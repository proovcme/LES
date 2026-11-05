"""Native service ownership with an isolated test API and real native Qdrant."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import httpx
import psutil
import pytest

from backend.light_processes import InstanceLock


@pytest.fixture
def executable():
    path=os.getenv('LES_TEST_NATIVE_RUNTIME')
    if not path:
        pytest.skip('Build les-runtime and set LES_TEST_NATIVE_RUNTIME for the live ownership gate')
    assert Path(path).is_file()
    return Path(path).resolve()


def wait_for(action, *, timeout=20):
    deadline=time.monotonic()+timeout
    while time.monotonic()<deadline:
        result=action()
        if result:
            return result
        time.sleep(.05)
    raise AssertionError('Isolated native runtime did not reach the expected state')


def status(path):
    try:
        return json.loads(path.read_text(encoding='utf-8'))
    except (OSError,ValueError):
        return {}


def assert_dead(pid):
    def dead():
        try:
            return not psutil.pid_exists(pid) or psutil.Process(pid).status()==psutil.STATUS_ZOMBIE
        except psutil.NoSuchProcess:
            return True
    wait_for(dead,timeout=5)


@pytest.fixture
def fixture_root(tmp_path):
    root=tmp_path/'Приложение 日本語'
    (root/'proxy').mkdir(parents=True)
    (root/'proxy/__init__.py').write_text('',encoding='utf-8')
    # SQLite marker verifies reuse of exactly the same state after an API crash.
    (root/'proxy/app.py').write_text('''import os,sqlite3,subprocess,sys
from pathlib import Path
from fastapi import FastAPI
app=FastAPI()
@app.get('/api/light/instance')
def instance():return {'instance_id':os.environ['LES_LIGHT_INSTANCE_ID']}
@app.get('/environment')
def environment():return {'foreign_present':any(os.getenv(key) for key in ('OPENAI_API_KEY','LLM_MODEL','OLLAMA_API_KEY','PYTHONHOME')),
                          'mutations':os.environ['LES_STARTUP_BACKGROUND_MUTATIONS']}
@app.post('/marker')
def write_marker():
    with sqlite3.connect(Path(os.environ['LES_STATE_ROOT'])/'data/probe.sqlite') as db:
        db.execute('CREATE TABLE IF NOT EXISTS marker (text TEXT)')
        db.execute('DELETE FROM marker');db.execute('INSERT INTO marker VALUES (?)',('Лес помнит 日本語',))
    return {'saved':True}
@app.get('/marker')
def read_marker():
    with sqlite3.connect(Path(os.environ['LES_STATE_ROOT'])/'data/probe.sqlite') as db:
        return {'text':db.execute('SELECT text FROM marker').fetchone()[0]}
@app.post('/spawn')
def spawn():
    child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(120)'])
    return {'pid':child.pid}
''',encoding='utf-8')
    (root/'sovushka_ng.py').write_text('''import json,os
from http.server import BaseHTTPRequestHandler,HTTPServer
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        data=json.dumps({'instance_id':os.environ['LES_LIGHT_INSTANCE_ID']}).encode()
        self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
    def log_message(self,*args):pass
HTTPServer(('127.0.0.1',int(os.environ['SOVUSHKA_UI_PORT'])),Handler).serve_forever()
''',encoding='utf-8')
    return root


def command(executable,root,state,qdrant):
    return [str(executable),'--root',str(root),'--state',str(state),'--python',sys.executable,
            '--qdrant',str(qdrant),'--parent-pipe','--read-only']


def test_contract_admits_remaining_python_core(executable):
    result=subprocess.run([str(executable),'--contract'],check=True,capture_output=True,text=True,timeout=5)
    contract=json.loads(result.stdout)
    assert contract['engine']=='rust' and contract['python_core_remaining'] is True


def test_native_and_previous_python_owner_locks_exclude_each_other(executable,fixture_root,tmp_path):
    state=tmp_path/'state';state.mkdir()
    marker=state/'launcher-status.json';marker.write_text('{"phase":"ready","marker":"do not overwrite"}')
    original=marker.read_bytes()
    lock=InstanceLock(state/'launcher.lock');assert lock.acquire()
    try:
        result=subprocess.run(command(executable,fixture_root,state,sys.executable),timeout=5)
        assert result.returncode==10
        assert marker.read_bytes()==original
        assert not (state/'logs').exists()
    finally:
        lock.release()


@pytest.fixture
def running(executable,fixture_root,tmp_path):
    qdrant=os.getenv('LES_TEST_NATIVE_QDRANT')
    if not qdrant:
        pytest.skip('Set LES_TEST_NATIVE_QDRANT for the isolated service ownership gate')
    state=tmp_path/'Состояние леса'
    log=(tmp_path/'owner.log').open('wb')
    process=subprocess.Popen(command(executable,fixture_root,state,Path(qdrant).resolve()),
        stdin=subprocess.PIPE,stdout=log,stderr=subprocess.STDOUT,
        env={**os.environ,'OPENAI_API_KEY':'foreign-fixture','LLM_MODEL':'foreign-fixture'},
        creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    target=state/'launcher-status.json'
    try:
        ready=wait_for(lambda: status(target) if status(target).get('phase')=='ready' else None,timeout=30)
        yield process,state,ready
    finally:
        if process.poll() is None:
            try:
                process.stdin.write(b'stop\n');process.stdin.flush();process.wait(timeout=10)
            except (OSError,subprocess.TimeoutExpired):
                process.kill();process.wait(timeout=5)
        if process.stdin:
            process.stdin.close()
        log.close()


def test_recovery_preserves_state_and_normal_stop_leaves_no_children(running):
    process,state,before=running
    assert before['launcher_engine']=='rust'
    with httpx.Client(timeout=3,trust_env=False) as client:
        assert client.post(before['api_url']+'/marker').json()['saved']
        assert client.get(before['api_url']+'/environment').json()=={'foreign_present':False,'mutations':'false'}
        configuration=json.loads((state/'qdrant/runtime.yaml').read_text())
        qdrant=f"http://127.0.0.1:{configuration['service']['http_port']}"
        headers={'api-key':configuration['service']['api_key']}
        client.put(qdrant+'/collections/proof',headers=headers,json={'vectors':{'size':2,'distance':'Cosine'}}).raise_for_status()
        client.put(qdrant+'/collections/proof/points?wait=true',headers=headers,json={
            'points':[{'id':1,'vector':[1.,0.],'payload':{'proof':'Лес помнит'}}]}).raise_for_status()
    lock=InstanceLock(state/'launcher.lock')
    assert not lock.acquire()  # Reverse direction: a Python launcher cannot take the native state.
    api=psutil.Process(before['api_pid']);assert api.ppid()==process.pid
    api.kill()
    after=wait_for(lambda: current if (current:=status(state/'launcher-status.json')).get('phase')=='ready'
                   and current.get('instance_id')!=before['instance_id'] else None,timeout=25)
    with httpx.Client(timeout=3,trust_env=False) as client:
        assert client.get(after['api_url']+'/marker').json()['text']=='Лес помнит 日本語'
        configuration=json.loads((state/'qdrant/runtime.yaml').read_text())
        qdrant=f"http://127.0.0.1:{configuration['service']['http_port']}"
        point=client.post(qdrant+'/collections/proof/points',headers={'api-key':configuration['service']['api_key']},
                          json={'ids':[1],'with_payload':True}).json()['result'][0]
        assert point['payload']['proof']=='Лес помнит'
    for key in ('api_pid','ui_pid','qdrant_pid'):
        assert_dead(before[key])
    process.stdin.write(b'stop\n');process.stdin.flush();process.wait(timeout=10)
    terminal=status(state/'launcher-status.json')
    assert terminal['phase']=='stopped' and terminal['reason']=='desktop_stop'
    for key in ('api_pid','ui_pid','qdrant_pid'):
        assert_dead(after[key])
    assert not (state/'qdrant/runtime.yaml').exists()
    assert lock.acquire();lock.release()
    events=[json.loads(line) for line in (state/'logs/native-runtime-lifecycle.jsonl').read_text().splitlines()]
    assert any(row['event']=='recovery_requested' for row in events)
    assert events[-1]['event']=='launcher_stopped'
    assert configuration['service']['api_key'] not in (state/'logs/native-runtime-lifecycle.jsonl').read_text()


def test_restart_storm_has_a_terminal_reason_and_leaves_no_services(running):
    process,state,current=running
    pids=set()
    for number in range(4):
        pids.update(current[key] for key in ('api_pid','ui_pid','qdrant_pid'))
        api=psutil.Process(current['api_pid']);assert api.ppid()==process.pid
        api.kill()
        if number<3:
            previous=current['instance_id']
            current=wait_for(lambda: row if (row:=status(state/'launcher-status.json')).get('phase')=='ready'
                and row.get('instance_id')!=previous else None,timeout=25)
    assert process.wait(timeout=10)==1
    terminal=status(state/'launcher-status.json')
    assert terminal['phase']=='error' and terminal['reason']=='RECOVERY_LIMIT'
    for pid in pids:
        assert_dead(pid)


def test_parent_pipe_eof_is_recorded_and_stops_services(running):
    process,state,ready=running
    process.stdin.close()
    assert process.wait(timeout=10)==0
    terminal=status(state/'launcher-status.json')
    assert terminal['phase']=='stopped' and terminal['reason']=='parent_pipe_eof'
    for key in ('api_pid','ui_pid','qdrant_pid'):
        assert_dead(ready[key])


@pytest.mark.skipif(sys.platform!='win32',reason='Abrupt owner death is a Windows Job contract')
def test_owner_crash_stops_owned_services_and_grandchildren(running):
    process,state,ready=running
    with httpx.Client(timeout=3,trust_env=False) as client:
        grandchild=client.post(ready['api_url']+'/spawn').json()['pid']
    assert psutil.Process(ready['api_pid']).ppid()==process.pid
    process.kill();process.wait(timeout=5)
    for pid in [grandchild,ready['api_pid'],ready['ui_pid'],ready['qdrant_pid']]:
        assert_dead(pid)


def test_native_asset_requires_current_source_receipt_and_never_overwrites(tmp_path,monkeypatch):
    from tools import build_native_runtime as builder
    monkeypatch.setattr(builder,'source_digest',lambda:'source-hash')
    source=tmp_path/'built/les-runtime.exe';source.parent.mkdir();source.write_bytes(b'fixture')
    receipt={'schema':'les.native-runtime-asset.v1','file':source.name,
             'sha256':hashlib.sha256(source.read_bytes()).hexdigest(),
             'source_sha256':'source-hash','remapped_paths':True}
    (source.parent/'runtime-receipt.json').write_text(json.dumps(receipt))
    target=builder.stage_asset(tmp_path/'payload',source)
    assert target.read_bytes()==source.read_bytes()
    with pytest.raises(FileExistsError):builder.stage_asset(target.parent,source)
    source.write_bytes(b'changed')
    with pytest.raises(ValueError,match='receipt'):builder.stage_asset(tmp_path/'bad',source)
    assert not (tmp_path/'bad').exists()
