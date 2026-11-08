"""Native skill library/host runs with no importable Python application and resumes confirmed rows."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import pytest


@pytest.fixture
def host(tmp_path):
    executable = os.getenv('LES_TEST_SKILL_HOST') or os.getenv('LES_TEST_NATIVE_API')
    if not executable:
        pytest.skip('Build les-api with --skill-host and set LES_TEST_SKILL_HOST')
    standalone = tmp_path / 'Нативный исполнитель 日本語.exe'
    import shutil
    shutil.copyfile(executable, standalone)
    library = tmp_path / 'Библиотека'; library.mkdir()
    package = library / 'calculation'; package.mkdir()
    (package/'SKILL.md').write_text('description: Проверяемый навык\n' + 'Я字' * 7000, encoding='utf-8')
    artifacts = tmp_path / 'Задания'
    def install(code):
        source = package/'calculate.py'; source.write_text(code,encoding='utf-8')
        approval = {'skill':'calculation','path':'calculate.py','entrypoint':'calculate',
            'sha256':hashlib.sha256(source.read_bytes()).hexdigest()}
        (library/'approved-compute.json').write_text(json.dumps([approval]),encoding='utf-8')
        return source
    environment = {name:value for name,value in os.environ.items()
        if name.upper() in {'SYSTEMROOT','WINDIR','TEMP','TMP'}}
    environment.update({'PYTHONPATH':str(tmp_path/'no-python-application'),
                        'LES_API_KEY':'synthetic-must-not-reach-skill'})
    def start(action='calculate', **fields):
        request = {'schema':'les.skills.v1','action':action,'program':os.getenv('LES_TEST_SKILL_PYTHON') or sys.executable,
            'library':str(library),'artifacts':str(artifacts),'skill':'calculation','path':'calculate.py',**fields}
        child = subprocess.Popen([str(standalone),'--skill-host'],cwd=tmp_path,env=environment,
            stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8',
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        return child, json.dumps(request,ensure_ascii=False)
    def run(action='calculate', **fields):
        child,request = start(action,**fields)
        try:
            stdout,stderr = child.communicate(request,timeout=35)
            assert child.returncode == 0,stderr
            result = json.loads(stdout)
            assert result['ok'] is True,result
            return result['value']
        finally:
            if child.poll() is None: child.kill();child.wait(timeout=5)
    return install,run,start,artifacts,library


def test_standalone_library_and_70_row_python_skill_have_no_backend_dependency(host):
    install,run,_,artifacts,library = host
    install('import os,sys,importlib.util\nfrom decimal import Decimal\ndef calculate(value):\n'
        '    print("diagnostic output must not corrupt protocol")\n'
        '    assert "backend" not in sys.modules and "proxy" not in sys.modules\n'
        '    assert sys.dont_write_bytecode\n'
        '    assert importlib.util.find_spec("backend") is None and importlib.util.find_spec("proxy") is None\n'
        '    assert "LES_API_KEY" not in os.environ and "PYTHONPATH" not in os.environ\n'
        '    return {"cost":str(Decimal(value["q"])*Decimal(value["price"]))}\n')
    assert run('catalog')['skills'][0]['skill']=='calculation'
    offset=0;parts=[]
    while offset is not None:
        page=run('read',path='SKILL.md',offset=offset);parts.append(page['text']);offset=page['next_offset']
    assert ''.join(parts)==(library/'calculation/SKILL.md').read_text(encoding='utf-8')
    result=run(items=[{'id':f'row-{i}','input':{'q':str(i),'price':'10.25'}} for i in range(1,71)])
    assert (result['completed'],result['succeeded'],result['interrupted'])==(70,70,False)
    offset=0;rows=[]
    while offset is not None:
        page=run('read_job',job_id=result['job_id'],offset=offset);rows+=page['rows'];offset=page['next_offset']
    assert rows[-1]['result']['cost']=='717.50'
    assert len(list((artifacts/result['job_id']).glob('[0-9][0-9][0-9][0-9].json')))==70


def test_previously_available_numeric_dependencies_work_without_application_or_site_startup(host):
    install,run,_,_,_ = host
    install('import sys\nimport numpy as np\nimport openpyxl\nimport importlib.util\n'
        'def calculate(value):\n'
        '    assert importlib.util.find_spec("backend") is None\n'
        '    book=openpyxl.Workbook();book.active["A1"]=value["n"]\n'
        '    return {"value":int(np.array([book.active["A1"].value,2]).sum())}\n')
    result=run(items=[{'id':'numeric','input':{'n':7}}])
    assert result['succeeded']==1, result
    assert result['rows'][0]['result']=={'value':9}


def test_killed_native_host_resumes_without_repeating_confirmed_rows(host):
    install,run,start,artifacts,_=host
    install('import time\nfrom pathlib import Path\ndef calculate(value):\n'
        '    if value["n"] == 1:\n'
        '        counter=Path("counter");counter.write_text(str(int(counter.read_text())+1) if counter.exists() else "1")\n'
        '    elif not Path("resume-once").exists():\n'
        '        Path("resume-once").write_text("ready");time.sleep(20)\n'
        '    return value\n')
    child,request=start(items=[{'id':'first','input':{'n':1}},{'id':'second','input':{'n':2}}])
    try:
        child.stdin.write(request);child.stdin.close()
        deadline=time.monotonic()+10
        while time.monotonic()<deadline:
            found=list(artifacts.glob('*/resume-once')) if artifacts.exists() else []
            if found: break
            assert child.poll() is None
            time.sleep(.02)
        assert found,'child did not checkpoint'
        work=found[0].parent
        worker_pid=json.loads((work/'execution.json').read_text())['worker_pid']
        child.kill();child.wait(timeout=5)
        if os.name=='nt':
            import psutil
            for _ in range(100):
                if not psutil.pid_exists(worker_pid):break
                time.sleep(.02)
            assert not psutil.pid_exists(worker_pid),'skill outlived its native owner'
        result=run('resume',job_id=work.name)
        assert (result['completed'],result['succeeded'],result['interrupted'])==(2,2,False)
        assert (work/'counter').read_text()=='1'
        assert result['rows'][0]['id']=='first' and result['rows'][1]['id']=='second'
    finally:
        if child.poll() is None:child.kill();child.wait(timeout=5)


def test_timeout_retains_reason_rows_and_missing_runtime_is_actionable(host):
    install,run,_,artifacts,_=host
    install('import time\ndef calculate(value):\n    if value.get("wait"): time.sleep(20)\n    return value\n')
    result=run(items=[{'id':'done','input':{}},{'id':'waiting','input':{'wait':True}}],timeout_ms=1000)
    assert result['completed']==1 and result['pending_ids']==['waiting']
    assert result['interrupted'] and result['error_code']=='SKILL_DEADLINE_EXCEEDED'
    assert json.loads((artifacts/result['job_id']/'stop.json').read_text())['code']=='SKILL_DEADLINE_EXCEEDED'
    result=run(items=[{'id':'not-run','input':{}}],program=str(artifacts/'missing-python.exe'))
    assert result['error_code']=='SKILL_RUNTIME_MISSING' and result['completed']==0
    assert 'исполнитель' in result['error_message']


def test_missing_dependency_is_named_and_saved_without_running_any_rows(host):
    install,run,_,artifacts,_=host
    install('import les_missing_dependency_for_test\ndef calculate(value): return value\n')
    result=run(items=[{'id':'not-run','input':{}}])
    assert result['completed']==0 and result['pending_ids']==['not-run']
    assert result['error_code']=='SKILL_DEPENDENCY_MISSING'
    assert 'les_missing_dependency_for_test' in result['error_message']
    saved=json.loads((artifacts/result['job_id']/'summary.json').read_text(encoding='utf-8'))
    assert saved['error_message']==result['error_message']
    failure=json.loads((artifacts/result['job_id']/'failure.json').read_text(encoding='utf-8'))
    assert 'les_missing_dependency_for_test' in failure['message']


@pytest.mark.skipif(os.name!='nt',reason='Windows dependency compatibility')
def test_windows_modules_use_explicit_paths_and_dlls_without_site_startup(host):
    program=os.getenv('LES_TEST_SKILL_PYTHON')
    if not program or not (Path(program).parent/'Lib/site-packages/win32').is_dir():
        pytest.skip('Prepared skill runtime with the existing pywin32 dependency required')
    install,run,_,_,_=host
    install('import sys,importlib.util\nimport win32api,pythoncom,pywintypes,win32com\n'
        'def calculate(value):\n'
        '    assert "site" not in sys.modules and sys.dont_write_bytecode\n'
        '    assert importlib.util.find_spec("backend") is None\n'
        '    return {"pid":win32api.GetCurrentProcessId(),"value":value["n"]+1}\n')
    result=run(items=[{'id':'windows','input':{'n':8}}])
    assert result['succeeded']==1,result
    assert result['rows'][0]['result']['value']==9 and result['rows'][0]['result']['pid']>0
