"""Rho drives real host effects; retired counter-only scheduling is not tested."""
import asyncio
import os
import pytest
from proxy.services.native_research_loop_service import run_research

@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Build the native coordinator')

def request(model, tool, maximum=3, deadline=30.):
    return run_research(allowed_tools=['agent_workspace'],max_calls=maximum,
        deadline_seconds=deadline,binding={'dataset_ids':[]},model=model,tool=tool)

def proposal(*calls):
    return {'response':{'text':'','tool_calls':[{'id':f'call-{i}',
        'function':{'name':'agent_workspace','arguments':args}} for i,args in enumerate(calls)]},'sources':[]}

def test_calls_total_repeated_failure_and_request_isolation():
    async def scenario(maximum):
        effects=[]
        async def model(_):return proposal({'operation':'save','area':'user','text':'x'})
        async def tool(call):
            effects.append(call)
            return {'payload':{'tool':'agent_workspace','operation':'save','status':'error','result':{}}}
        result=await request(model,tool,maximum=maximum)
        assert result['status']=='needs_final'
        assert len(effects)==min(1,maximum)
        assert result['remaining']==max(0,maximum-1)
    asyncio.run(scenario(0));asyncio.run(scenario(2));asyncio.run(scenario(1))

def test_duplicate_batch_is_skipped_and_revision_advance_allows_rereading():
    async def scenario():
        effects=[]
        async def model(_):return proposal({'operation':'read','area':'user'},{'operation':'read','area':'user'})
        async def tool(call):
            effects.append(call)
            return {'payload':{'tool':'agent_workspace','operation':'read','status':'ok','result':{}}}
        result=await request(model,tool)
        assert result['status']=='needs_final' and len(effects)==1
        assert result['remaining']==2
    asyncio.run(scenario())

def test_real_monotonic_deadline_blocks_tools_after_slow_selector():
    async def scenario():
        async def model(_):
            await asyncio.sleep(.1)
            return proposal({'operation':'save','area':'user','text':'late'})
        async def tool(_):pytest.fail('Deadline-expired effect executed')
        with pytest.raises(Exception) as error:
            await request(model,tool,deadline=.04)
        assert error.value.detail['code']=='RESEARCH_DEADLINE_EXCEEDED'
    asyncio.run(scenario())

def test_main_loop_binding_still_uses_native_state_and_receipts(tmp_path,monkeypatch):
    from proxy.services import agent_state_service as state,chat_session_service as sessions
    from proxy.services.tool_harness_service import ToolHarness
    from proxy.services.web_research_config_service import WebResearchConfig
    from proxy.services import agent_workspace_tools as tools
    root=tmp_path/'State 日本語';(root/'data').mkdir(parents=True);db=str(root/'data/meta.db')
    monkeypatch.setenv('LES_PRODUCT_EDITION','light');monkeypatch.setenv('LES_STATE_ROOT',str(root))
    monkeypatch.setattr(state,'rag_meta_db_path',lambda:db);monkeypatch.setattr(sessions,'rag_meta_db_path',lambda:db)
    harness=ToolHarness(web_config=WebResearchConfig('off','','',''));tools.bind(harness,sessions.create_session()['session_id'])
    async def scenario():
        args={'operation':'save','area':'user','text':'Коротко 🌲','expected_revision':'missing'}
        async def model(_):return proposal(args,args)
        async def tool(call):return {'payload':await harness.call_async(call['tool'],call['args'])}
        result=await request(model,tool)
        assert result['status']=='needs_final' and result['remaining']==2
    asyncio.run(scenario())
    assert state.call('read',area='user')['text']=='Коротко 🌲'
    assert len(state.call('history',area='user')['items'])==1
