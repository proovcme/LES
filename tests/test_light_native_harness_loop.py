"""Real native loop control with actual bound local tools; no simulated model pass."""
import os
import time
import pytest
from proxy.services.harness_loop_service import HarnessLoop
from proxy.services.chat_evidence_tools import tool_call_identity


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Build native harness for its integration gate')


def test_calls_total_repeated_failure_and_request_isolation():
    zero = HarnessLoop(max_calls=0,deadline_seconds=30.)
    assert not zero.enter_round() and zero.reason == 'calls_budget'
    loop = HarnessLoop(max_calls=2,deadline_seconds=30.)
    identity = tool_call_identity({'tool':'read_source','args':{'page':2}})
    assert loop.enter_round() and loop.round == 1
    assert loop.select([identity]) == [0]
    assert loop.begin(identity)
    loop.finish(failed=True)
    assert loop.remaining == 1 and loop.enter_round()
    assert loop.select([identity]) == []
    assert loop.stopped and loop.reason == 'repeated_tool_failure'
    independent = HarnessLoop(max_calls=1,deadline_seconds=30.)
    assert independent.enter_round() and independent.begin(identity)
    independent.finish(failed=False)
    assert independent.remaining == 0 and independent.reason == 'calls_budget'
    assert not independent.enter_round()


def test_duplicate_batch_is_skipped_and_revision_advance_allows_rereading():
    loop = HarnessLoop(max_calls=4,deadline_seconds=30.)
    read = 'agent:0:'+tool_call_identity({'tool':'agent_workspace','args':{'operation':'read','area':'user'}})
    assert loop.enter_round()
    assert loop.select([read,read]) == [0,1]
    assert loop.begin(read)
    loop.finish(failed=False)
    assert not loop.begin(read)  # Already completed earlier in the same response.
    assert loop.remaining == 3 and not loop.stopped
    assert loop.enter_round()
    changed = read.replace('agent:0:','agent:1:')
    assert loop.select([changed]) == [0] and loop.begin(changed)
    loop.finish(failed=False)
    assert loop.select([changed]) == [] and loop.reason == 'repeated_read'


def test_real_monotonic_deadline_blocks_tools_after_slow_selector():
    loop = HarnessLoop(max_calls=2,deadline_seconds=.05)
    time.sleep(.07)
    assert loop.select(['write']) == []
    assert loop.reason == 'deadline'
    assert not loop.begin('write')
    assert loop.remaining == 2


def test_main_loop_binding_still_uses_native_state_and_receipts(tmp_path,monkeypatch):
    from proxy.services import agent_state_service as state, chat_session_service as sessions
    from proxy.services.tool_harness_service import ToolHarness
    from proxy.services.web_research_config_service import WebResearchConfig
    from proxy.services import agent_workspace_tools as tools
    root = tmp_path/'State 日本語'; (root/'data').mkdir(parents=True)
    db = str(root/'data/meta.db')
    monkeypatch.setenv('LES_PRODUCT_EDITION','light');monkeypatch.setenv('LES_STATE_ROOT',str(root))
    monkeypatch.setattr(state,'rag_meta_db_path',lambda:db)
    monkeypatch.setattr(sessions,'rag_meta_db_path',lambda:db)
    harness = ToolHarness(web_config=WebResearchConfig('off','','',''))
    tools.bind(harness,sessions.create_session()['session_id'])
    call = dict(tool='agent_workspace',args=dict(operation='save',area='user',text='Коротко 🌲',expected_revision='missing'))
    identity = tool_call_identity(call)
    loop = HarnessLoop(max_calls=3,deadline_seconds=30.)
    assert loop.enter_round()
    executed = []
    for index in loop.select([identity,identity]):
        if not loop.begin(identity):continue
        result = harness.call(call['tool'],call['args'])
        executed.append(result)
        loop.finish(failed=result['status']=='error')
    assert len(executed)==1 and executed[0]['result']['persisted']
    assert loop.remaining == 2
    assert state.call('read',area='user')['text']=='Коротко 🌲'
    assert len(state.call('history',area='user')['items'])==1
