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
