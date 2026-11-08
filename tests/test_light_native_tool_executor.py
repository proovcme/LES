"""Native admission/ledger protects effects, replays and actor-bound whole results."""
import asyncio
from dataclasses import replace
import hashlib
import json
import math
import random
import struct
import time

import pytest
from backend import native_core
from proxy.services.tool_contract_service import EffectClass,IdempotencyPolicy,RetryPolicy,ResultBudget,ToolContract
from proxy.services.tool_registry_service import ToolRegistry,ToolRegistration
from proxy.services.trusted_executor_service import ApprovalReceipt,ExecutionRequest,SqliteExecutionStore,TrustedExecutor,argument_sha256


def registry(handler, *, effect=EffectClass.READ, scopes=('dataset',), max_chars=10000, max_items=1000):
    contract=ToolContract(name='test_record',version='1.0.0',title='Test',category='test',summary='Test record',
        input_schema={'type':'object','properties':{'dataset_id':{'type':'string'},'proposal_revision':{'type':'string'}},
                      'required':['dataset_id'],'additionalProperties':True},
        result_schema='les_tool_result_v1',effect=effect,scopes=scopes,timeout_seconds=5,
        retry=RetryPolicy.NEVER,idempotency=IdempotencyPolicy.NONE,
        result_budget=ResultBudget(max_chars=max_chars,max_items=max_items),model_owned_fields=(),provenance='Synthetic test')
    registry=ToolRegistry();registry.register(ToolRegistration(contract=contract,handler=handler));return registry


def payload(arguments):
    return {'schema':'les_tool_result_v1','tool':'test_record','operation':'read','inputs':[],
        'status':'ok','result':dict(arguments),'sources':[],'missing':[],'warnings':[],'trace':'synthetic',
        'decision_required_from_model':False}


def request(arguments=None, **fields):
    return replace(ExecutionRequest(call_id='first',tool_name='test_record',arguments=arguments or {'dataset_id':'allowed'},
        allowed_dataset_ids=('allowed',),actor_id='alice',actor_role='admin',approval_receipt_id=None,
        idempotency_key=None,deadline_monotonic=time.monotonic()+10),**fields)


def approve(store,arguments,receipt='approval'):
    store.issue_approval(ApprovalReceipt(receipt_id=receipt,status='approved',proposal_revision='r1',
        tool_name='test_record',argument_sha256=argument_sha256(arguments),actor_id='alice',expires_at_epoch=time.time()+60))


def test_schema_scope_shadow_and_local_permission_never_reach_handler():
    calls=[]
    def handler(arguments):calls.append(arguments);return payload(arguments)
    executor=TrustedExecutor(registry(handler))
    invalid=asyncio.run(executor.execute(request({'dataset_id':3})))
    assert invalid.code=='TOOL_SCHEMA_INVALID'
    escaped=asyncio.run(executor.execute(request({'dataset_id':'allowed','nested':{'dataset_ids':['outside']}})))
    assert escaped.code=='TOOL_SCOPE_VIOLATION'
    executor.close()
    shadow=TrustedExecutor(registry(handler,effect=EffectClass.COMMIT))
    assert asyncio.run(shadow.execute(request(shadow=True))).code=='TOOL_WOULD_EXECUTE';shadow.close()
    local=TrustedExecutor(registry(handler,effect=EffectClass.LOCAL_STATE))
    assert asyncio.run(local.execute(request({'dataset_id':'allowed','local_state_allowed':True}))).code=='TOOL_LOCAL_STATE_PERMISSION_REQUIRED'
    assert calls==[];local.close()


@pytest.mark.asyncio
async def test_atomic_approval_binding_blocks_concurrent_and_cross_key_duplicate(tmp_path):
    store=SqliteExecutionStore(tmp_path/'ledger.db');arguments={'dataset_id':'allowed','proposal_revision':'r1'}
    approve(store,arguments)
    entered=asyncio.Event();release=asyncio.Event();calls=[]
    async def effect(value):calls.append(value);entered.set();await release.wait();return payload(value)
    first=TrustedExecutor(registry(effect,effect=EffectClass.COMMIT),store=store)
    second=TrustedExecutor(registry(effect,effect=EffectClass.COMMIT),store=store)
    req=request(arguments,approval_receipt_id='approval',idempotency_key='one-operation')
    task=asyncio.create_task(first.execute(req));await asyncio.wait_for(entered.wait(),2)
    duplicate=await second.execute(replace(req,call_id='concurrent'))
    assert duplicate.code=='TOOL_EXECUTION_IN_PROGRESS' and len(calls)==1
    release.set();result=await task;assert result.code=='TOOL_OK'
    replay=await second.execute(replace(req,call_id='replay'))
    assert replay.to_dict()==result.to_dict() and len(calls)==1
    cross_key=await second.execute(replace(req,idempotency_key='second-operation'))
    assert cross_key.code=='TOOL_APPROVAL_ALREADY_USED'
    store.revoke_approval('approval')
    assert (await second.execute(req)).code=='TOOL_APPROVAL_REQUIRED'
    first.close();second.close()


@pytest.mark.asyncio
async def test_lost_effect_reply_is_ambiguous_across_executor_restart(tmp_path):
    store=SqliteExecutionStore(tmp_path/'ledger.db');arguments={'dataset_id':'allowed','proposal_revision':'r1'}
    approve(store,arguments);calls=[]
    async def interrupted(value):calls.append('effect already happened');await asyncio.sleep(1);return payload(value)
    executor=TrustedExecutor(registry(interrupted,effect=EffectClass.EXTERNAL),store=store)
    req=request(arguments,approval_receipt_id='approval',idempotency_key='operation',deadline_monotonic=time.monotonic()+.1)
    assert (await executor.execute(req)).code=='TOOL_DEADLINE_EXCEEDED';executor.close()
    reopened=TrustedExecutor(registry(interrupted,effect=EffectClass.EXTERNAL),store=SqliteExecutionStore(tmp_path/'ledger.db'))
    assert (await reopened.execute(replace(req,deadline_monotonic=time.monotonic()+3))).code=='TOOL_EXECUTION_AMBIGUOUS'
    assert calls==['effect already happened'];reopened.close()


@pytest.mark.asyncio
async def test_whole_result_reference_is_actor_bound_persistent_and_expires(tmp_path):
    store=SqliteExecutionStore(tmp_path/'ledger.db')
    executor=TrustedExecutor(registry(payload,max_chars=100,max_items=2),store=store,cursor_ttl_seconds=.15)
    result=await executor.execute(request({'dataset_id':'allowed','rows':['Я字']*20}))
    assert result.code=='TOOL_RESULT_BUDGET_EXCEEDED' and result.cursor
    assert executor.resolve_cursor(result.cursor,actor_id='alice')['result']['rows']==['Я字']*20
    assert executor.resolve_cursor(result.cursor,actor_id='bob') is None;executor.close()
    reopened=TrustedExecutor(registry(payload,max_chars=100,max_items=2),store=SqliteExecutionStore(tmp_path/'ledger.db'))
    assert reopened.resolve_cursor(result.cursor,actor_id='alice')['tool']=='test_record'
    await asyncio.sleep(.17)
    assert reopened.resolve_cursor(result.cursor,actor_id='alice') is None;reopened.close()


def test_invalid_tool_result_does_not_become_success():
    def invalid(arguments):
        value=payload(arguments);value['decisions']=['missing_as_zero'];return value
    executor=TrustedExecutor(registry(invalid))
    assert asyncio.run(executor.execute(request())).code=='TOOL_RESULT_INVALID';executor.close()


def test_native_fingerprints_match_independent_python_json_on_float_edges_and_unicode():
    randomizer=random.Random(821)
    values=[-0.0,1e-7,1e-5,1e-4,1e16,1e20,1e23,0.1,1.2345678901234567,2**100,
        {'Я字':{'b':[True,None,'🦉\n'], 'a':1.25}}]
    for _ in range(10000):
        value=struct.unpack('<d',randomizer.randbytes(8))[0]
        if math.isfinite(value):values.append(value)
    arguments={'values':values,'a':'UTF-8'}
    expected=json.dumps(arguments,sort_keys=True,ensure_ascii=False,separators=(',',':'),allow_nan=False)
    native=native_core.required('tool_execution',request={'schema':'les.tool-execution.v1','action':'hash','arguments':arguments})['value']
    if native['canonical']!=expected:
        for value in values:
            one=native_core.required('tool_execution',request={'schema':'les.tool-execution.v1','action':'hash','arguments':{'v':value}})['value']['canonical']
            golden=json.dumps({'v':value},sort_keys=True,ensure_ascii=False,separators=(',',':'),allow_nan=False)
            assert one==golden
    assert native['sha256']==hashlib.sha256(expected.encode()).hexdigest()


def test_reported_tool_failure_preserves_cause_in_durable_receipt_and_replay(tmp_path):
    calls=[]
    def fail(arguments):
        calls.append(arguments)
        return {**payload(arguments), 'status':'error', 'result':{'code':'PDF_PAGE_OUT_OF_RANGE'}}
    store=SqliteExecutionStore(tmp_path/'ledger.db')
    executor=TrustedExecutor(registry(fail,effect=EffectClass.EXTERNAL),store=store)
    arguments={'dataset_id':'allowed','proposal_revision':'r1'}
    approve(store,arguments)
    req=request(arguments,approval_receipt_id='approval',idempotency_key='reported-error')
    outcome=asyncio.run(executor.execute(req))
    assert outcome.code=='PDF_PAGE_OUT_OF_RANGE' and outcome.status=='error'
    assert outcome.result['status']=='error'
    executor.close()
    reopened=TrustedExecutor(registry(fail,effect=EffectClass.EXTERNAL),store=SqliteExecutionStore(tmp_path/'ledger.db'))
    repeated=asyncio.run(reopened.execute(replace(req,deadline_monotonic=time.monotonic()+5)))
    assert repeated.code==outcome.code and repeated.result==outcome.result and len(calls)==1
    reopened.close()
