"""Actual lifecycle reasons are persisted separately from document/inference logs."""
import io
import json
from types import SimpleNamespace

import pytest
from tools import light_launcher
from backend.runtime_events import RuntimeJournal


@pytest.mark.parametrize('command,reason',[('stop\n','desktop_stop'),('','parent_pipe_eof')])
def test_parent_stop_and_desktop_loss_are_distinct_and_never_leave_ready(tmp_path,monkeypatch,command,reason):
    class Stack:
        def __init__(self,*args,**kwargs): pass
        def start(self,stop):
            assert stop.wait(2)
            raise InterruptedError()
        def stop(self): pass
    monkeypatch.setattr(light_launcher,'LightStack',Stack)
    monkeypatch.setattr(light_launcher,'attach_lifetime_job',lambda:None)
    monkeypatch.setattr(light_launcher.signal,'signal',lambda *args:None)
    monkeypatch.setattr(light_launcher.faulthandler,'enable',lambda:None)
    monkeypatch.setattr(light_launcher.sys,'stdin',io.StringIO(command))
    args=SimpleNamespace(root=str(tmp_path),state=str(tmp_path/'state'),qdrant=str(tmp_path/'fake.exe'),parent_pipe=True,read_only=True,browser=False)
    assert light_launcher.run(args)==0
    status=json.loads((tmp_path/'state/launcher-status.json').read_text(encoding='utf-8'))
    assert status['phase']=='stopped' and status['reason']==reason
    records=[json.loads(line) for line in (tmp_path/'state/logs/lifecycle.jsonl').read_text(encoding='utf-8').splitlines()]
    assert records[-1]['event']=='launcher_stopped' and records[-1]['exit_code']==0
    assert records[-1]['reason']==reason


def test_service_failure_keeps_exact_exit_code(tmp_path):
    stack=light_launcher.LightStack(tmp_path,tmp_path,tmp_path/'qdrant.exe')
    stack.children=[SimpleNamespace(pid=123,poll=lambda:42),SimpleNamespace(pid=124,poll=lambda:None)]
    stack.qdrant._process=SimpleNamespace(pid=125,poll=lambda:None)
    assert stack.failed()
    assert stack.last_failure=={'reason':'service_exit','services':[{'service':'api','child_pid':123,'exit_code':42}]}


def test_lifecycle_rotation_keeps_valid_json_and_reason_codes(tmp_path):
    journal=RuntimeJournal(tmp_path)
    journal.handler.maxBytes=250
    for attempt in range(12):
        journal.record('recovery_requested',reason='service_exit',attempt=attempt,exit_code=17)
    journal.close()
    files=list((tmp_path/'logs').glob('lifecycle.jsonl*'))
    assert 1<len(files)<=4
    for path in files:
        for line in path.read_text(encoding='utf-8').splitlines():
            row=json.loads(line)
            assert row['reason']=='service_exit' and row['exit_code']==17
