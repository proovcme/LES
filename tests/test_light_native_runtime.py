"""Native service ownership with real Axum and Qdrant in an isolated profile."""
import hashlib
import json
import os
from pathlib import Path
import shutil
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


def prepare_frontend(root):
    frontend = root/'desktop/light/frontend/dist'
    frontend.mkdir(parents=True)
    page = b'<!doctype html><title>LES native fixture</title>'
    (frontend/'index.html').write_bytes(page)
    (frontend/'build-receipt.json').write_text(json.dumps({
        'schema': 'les.frontend.v1', 'files': {'index.html': hashlib.sha256(page).hexdigest()},
    }), encoding='utf-8')
    return page


def stage_api(root, api):
    binary = root.parent/'native/api'/('les-api.exe' if os.name == 'nt' else 'les-api')
    binary.parent.mkdir(parents=True)
    try:
        os.link(api, binary)
    except OSError:
        shutil.copyfile(api, binary)
    return binary


@pytest.fixture
def fixture_root(tmp_path):
    api = os.getenv('LES_TEST_NATIVE_API')
    if not api:
        pytest.skip('Build the native API for the Windows service test')
    root = tmp_path/'Приложение 日本語'
    prepare_frontend(root)
    stage_api(root, api)
    return root


def command(executable, root, state, qdrant):
    return [str(executable), '--root', str(root), '--state', str(state),
            '--qdrant', str(qdrant), '--parent-pipe', '--read-only']


def test_contract_distinguishes_python_paths(executable):
    result=subprocess.run([str(executable),'--contract'],check=True,capture_output=True,text=True,timeout=5)
    contract=json.loads(result.stdout)
    assert contract['engine'] == 'rust'
    assert contract['python'] == {'native_application': False, 'external_skills': True, 'compatibility_host': False}


def test_native_only_start_uses_rust_ui_without_python(executable,tmp_path):
    api=os.getenv('LES_TEST_EDGE_API')
    if not api:
        pytest.skip('Build the Edge API')
    root=tmp_path/'application'
    page = prepare_frontend(root)
    binary = stage_api(root, api)
    state=tmp_path/'state'
    state.mkdir()
    (state/'.env').write_text('MAIL_IMAP_POLL_SEC=0\n',encoding='utf-8')
    args=[str(executable),'--root',str(root),'--state',str(state),
          '--qdrant',str(tmp_path/'unused-qdrant.exe'),'--edge-storage',str(state/'edge'),
          '--native-only','--parent-pipe']
    owner=subprocess.Popen(args,stdin=subprocess.PIPE,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,
                           creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
    try:
        marker=state/'launcher-status.json'
        ready=wait_for(lambda: value if (value:=status(marker)).get('phase')=='ready' else None)
        assert ready['native_only'] is True
        assert ready['ui_pid'] is None and ready['legacy_api_pid'] is None
        with httpx.Client(base_url=ready['api_url'],trust_env=False,timeout=5) as client:
            assert client.get('/app/').raise_for_status().content==page
            assert client.get('/api/light/instance').raise_for_status().json()=={'instance_id':ready['instance_id']}
            options=client.get('/api/light/frontend').raise_for_status().json()
            assert options['native_mail'] is True and options['legacy_url'] is None
            assert client.get('/api/mail/accounts').raise_for_status().json()['accounts']==[]
            runtime=client.get('/api/status').raise_for_status().json()
            assert runtime['engine']=='rust' and runtime['chat_admission']['code']=='ANSWER_MODEL_NOT_ASSIGNED'
            assert client.get('/api/health').raise_for_status().json()['rag']['status']=='empty'
            assert client.get('/api/jobs/summary').raise_for_status().json()['jobs']==[]
        children=psutil.Process(owner.pid).children(recursive=True)
        assert {p.pid:p.name() for p in children if p.name().lower()!='conhost.exe'}=={ready['api_pid']:binary.name}
    finally:
        owner.stdin.close()
        try:
            owner.wait(timeout=12)
        except subprocess.TimeoutExpired:
            owner.terminate()
            owner.wait(timeout=5)


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
    state = tmp_path/'Состояние леса'
    state.mkdir()
    (state/'.env').write_text(
        'TRUSTED_NETWORKS=127.0.0.1/32\nTRUSTED_NETWORK_ROLE=admin\nMAIL_IMAP_POLL_SEC=0\n',
        encoding='utf-8',
    )
    log=(tmp_path/'owner.log').open('wb')
    process=subprocess.Popen(command(executable,fixture_root,state,Path(qdrant).resolve()),
        stdin=subprocess.PIPE,stdout=log,stderr=subprocess.STDOUT,
        env={**os.environ,'OPENAI_API_KEY':'foreign-fixture','LLM_MODEL':'foreign-fixture','PYTHONHOME':'foreign-fixture'},
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
        created = client.post(before['api_url']+'/api/workspace/sessions',
                              json={'title': 'Лес помнит 日本語'}).raise_for_status()
        session = created.json()['session_id']
        environment = psutil.Process(before['api_pid']).environ()
        assert set(environment).isdisjoint({'PYTHONHOME', 'OPENAI_API_KEY', 'LLM_MODEL'})
        assert environment['LES_STARTUP_BACKGROUND_MUTATIONS'] == 'false'
        assert before['ui_pid'] is None and before['legacy_api_pid'] is None
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
        response = client.get(after['api_url']+f'/api/workspace/sessions/{session}').raise_for_status()
        assert response.json()['title'] == 'Лес помнит 日本語'
        configuration=json.loads((state/'qdrant/runtime.yaml').read_text())
        qdrant=f"http://127.0.0.1:{configuration['service']['http_port']}"
        point=client.post(qdrant+'/collections/proof/points',headers={'api-key':configuration['service']['api_key']},
                          json={'ids':[1],'with_payload':True}).json()['result'][0]
        assert point['payload']['proof']=='Лес помнит'
    for key in ('api_pid','qdrant_pid'):
        assert_dead(before[key])
    process.stdin.write(b'stop\n');process.stdin.flush();process.wait(timeout=10)
    terminal=status(state/'launcher-status.json')
    assert terminal['phase']=='stopped' and terminal['reason']=='desktop_stop'
    for key in ('api_pid','qdrant_pid'):
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
        pids.update(current[key] for key in ('api_pid','qdrant_pid'))
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
    for key in ('api_pid','qdrant_pid'):
        assert_dead(ready[key])


@pytest.mark.skipif(sys.platform!='win32',reason='Abrupt owner death is a Windows Job contract')
def test_owner_crash_stops_owned_services_and_grandchildren(running):
    process,state,ready=running
    from concurrent.futures import ThreadPoolExecutor

    marker = state/'mcp-child.pid'
    script = state/'wait_mcp.py'
    script.write_text(
        'import os, time\nfrom pathlib import Path\n'
        'Path(__file__).with_name("mcp-child.pid").write_text(str(os.getpid()))\n'
        'time.sleep(60)\n', encoding='utf-8',
    )
    with httpx.Client(base_url=ready['api_url'], timeout=12, trust_env=False) as client:
        saved = client.post('/api/mcp', json={
            'name': 'Owned stdio', 'transport': 'stdio', 'command': sys.executable, 'args': [str(script)],
        }).raise_for_status().json()
        with ThreadPoolExecutor(max_workers=1) as pool:
            from urllib.parse import quote
            request = pool.submit(client.post, '/api/mcp/'+quote(saved['id'], safe='')+'/probe')
            grandchild = wait_for(lambda: marker.read_text() if marker.is_file() else None, timeout=10)
            assert psutil.Process(ready['api_pid']).ppid() == process.pid
            process.kill()
            process.wait(timeout=5)
            with pytest.raises(httpx.TransportError):
                request.result(timeout=5)
    for pid in [int(grandchild), ready['api_pid'], ready['qdrant_pid']]:
        assert_dead(pid)


def test_packaged_skill_interpreter_is_selected_without_app_imports_and_corruption_never_falls_back(
        executable,fixture_root,tmp_path,request):
    program=os.getenv('LES_TEST_SKILL_PYTHON')
    if not program:pytest.skip('Prepared portable skill Python required')
    from tools.skill_python_asset import stage
    target=fixture_root.parent/'native/skill-python'
    stage(target,Path(program).parent)
    (target.parent/'skill-python-required.json').write_text('{"required":true}',encoding='utf-8')
    process,state,ready=request.getfixturevalue('running')
    selected = Path(psutil.Process(ready['api_pid']).environ()['LES_SKILL_PYTHON'])
    assert selected == target/'python.exe'
    probe = subprocess.run([str(selected), '-I', '-S', '-B', '-c',
        'import sys,json,importlib.util; startup_site="site" in sys.modules; '
        'import numpy,openpyxl; print(json.dumps({"site_startup":startup_site,'
        '"bytecode_writes":not sys.dont_write_bytecode,'
        '"application_imports":importlib.util.find_spec("proxy") is not None,"numpy":numpy.__version__}))'],
        capture_output=True, text=True, encoding='utf-8', check=True, timeout=20)
    details = json.loads(probe.stdout)
    assert details['site_startup'] is False
    assert details['bytecode_writes'] is False
    assert details['application_imports'] is False
    assert details['numpy']
    process.stdin.write(b'stop\n');process.stdin.flush();process.wait(timeout=10)
    stdlib=next(target.glob('python*.zip'));stdlib.write_bytes(b'corrupt fixture')
    fresh_state=tmp_path/'must-not-start'
    result=subprocess.run(command(executable,fixture_root,fresh_state,Path(os.environ['LES_TEST_NATIVE_QDRANT'])),
        capture_output=True,timeout=10)
    assert result.returncode!=0 and b'SKILL_RUNTIME_INVALID' in result.stdout+result.stderr
    assert not fresh_state.exists()


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
