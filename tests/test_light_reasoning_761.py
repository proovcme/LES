import json
import pytest
import httpx
from proxy.services.model_reasoning_service import ReasoningOutput, profile_execution_preset, validate_reasoning_policy
from proxy.services.model_execution_preset_service import _FACTORY_9B
from proxy.services.openai_compatible_transport_service import InferenceRequest, ModelTransportError
from test_openai_compatible_transport_service import _resolved, _transport


def test_profile_budget_reserved_without_changing_factory():
    enabled=profile_execution_preset(_FACTORY_9B, {'reasoning_enabled':True,'reasoning_budget_tokens':2048})
    assert enabled.reasoning_enabled and enabled.generation_reserve_tokens==2048
    assert not _FACTORY_9B.reasoning_enabled and _FACTORY_9B.generation_reserve_tokens==1200
    assert enabled.diagnostics()['reasoning']['requested'] is True
    with pytest.raises(ValueError,match='контекст'):
        profile_execution_preset(_FACTORY_9B, {'reasoning_enabled':True,'reasoning_budget_tokens':8192})


@pytest.mark.parametrize('policy',[{'reasoning_enabled':'false'},{'reasoning_budget_tokens':True},{'reasoning_budget_tokens':511},{'reasoning_budget_tokens':8193}])
def test_invalid_reasoning_policy_rejected(policy):
    with pytest.raises(ValueError):validate_reasoning_policy(policy)


@pytest.mark.parametrize('pieces',[
    ['<th','ink>private','</thi','nk>Visible',' answer'],
    ['prefilled private thought','</thi','nk>Visible answer'],
])
def test_inline_reasoning_split_at_any_boundary(pieces):
    output=ReasoningOutput(True)
    assert ''.join(output.feed(p) for p in pieces)+output.finish('stop')=='Visible answer'
    assert output.observed


def test_unfinished_reasoning_never_becomes_an_answer():
    output=ReasoningOutput(True)
    assert output.feed('<think>private')==''
    with pytest.raises(ValueError,match='REASONING_BUDGET_EXHAUSTED'):output.finish('length')
    output=ReasoningOutput(True)
    output.feed('prefilled thought')
    with pytest.raises(ValueError):output.finish('length')


@pytest.mark.asyncio
async def test_stream_reasoning_transport_and_separation(tmp_path):
    bodies=[]
    payload=''.join('data: '+json.dumps({'choices':[{'delta':{'content':t}}]})+'\n\n' for t in ['<th','ink>private','</thi','nk>Visible'])+'data: '+json.dumps({'choices':[{'delta':{},'finish_reason':'stop'}]})+'\n\ndata: [DONE]\n\n'
    def handler(req):
        bodies.append(json.loads(req.content));return httpx.Response(200,text=payload)
    transport,client=_transport(tmp_path,handler)
    try:
        events=[e async for e in transport.stream(_resolved(),InferenceRequest(messages=[{'role':'user','content':'question'}],max_output_tokens=2048,reasoning_enabled=True))]
    finally:await client.aclose()
    assert bodies[0]['chat_template_kwargs']['enable_thinking'] is True
    assert bodies[0]['max_tokens']==2048
    assert ''.join(e.text for e in events)=='Visible'


@pytest.mark.asyncio
async def test_reasoning_only_length_is_explicit_error(tmp_path):
    payload='data: '+json.dumps({'choices':[{'delta':{'content':'<think>private'},'finish_reason':'length'}]})+'\n\n'
    transport,client=_transport(tmp_path,lambda req:httpx.Response(200,text=payload))
    try:
        with pytest.raises(ModelTransportError,match='REASONING_BUDGET_EXHAUSTED'):
            [e async for e in transport.stream(_resolved(),InferenceRequest(messages=[],max_output_tokens=512,reasoning_enabled=True))]
    finally:await client.aclose()


@pytest.mark.asyncio
async def test_native_thinking_is_progress_and_final_text_only(tmp_path):
    bodies = []
    frames = [
        {'message': {'thinking': 'internal computation', 'content': ''}, 'done': False},
        {'message': {'content': 'Visible answer'}, 'done': False},
        {'message': {'content': ''}, 'done': True, 'done_reason': 'stop'},
    ]
    def handler(request):
        bodies.append(json.loads(request.content))
        return httpx.Response(200, text=''.join(json.dumps(frame)+'\n' for frame in frames))
    transport, client = _transport(tmp_path, handler)
    events = []
    async def sink(event):
        events.append(event)
    try:
        result = await transport.complete(_resolved(chat_protocol='native_chat_v1'),
            InferenceRequest(messages=[], max_output_tokens=2048, reasoning_enabled=True), token_sink=sink)
    finally:
        await client.aclose()
    assert bodies[0]['think'] is True
    assert result.text == 'Visible answer'
    assert any(e['event']=='progress' and e['data']['stage']=='thinking' for e in events)
    assert ''.join(e['data'] for e in events if e['event']=='token') == result.text
    assert 'internal computation' not in json.dumps(events)


@pytest.mark.parametrize('separate', [True, False])
def test_separate_reasoning_only_budget_is_explicit(separate):
    output = ReasoningOutput(True)
    if separate:
        output.separate()
        output.feed(' ')
    else:
        output.feed('<think>internal</think>')
    with pytest.raises(ValueError, match='REASONING_BUDGET_EXHAUSTED'):
        output.finish('length')


@pytest.mark.asyncio
async def test_native_thought_counts_against_response_limit(tmp_path):
    transport, client = _transport(tmp_path, lambda request: httpx.Response(200,
        text=json.dumps({'message': {'thinking': 'x'*65}, 'done': True})+'\n'), body_limit=64)
    try:
        with pytest.raises(ModelTransportError, match='UPSTREAM_RESPONSE_TOO_LARGE'):
            await transport.complete(_resolved(chat_protocol='native_chat_v1'),
                InferenceRequest(messages=[], max_output_tokens=2048, reasoning_enabled=True))
    finally:
        await client.aclose()


def test_reasoning_policy_is_immutable_for_existing_chat(tmp_path):
    from proxy.services import chat_profile_service as profiles
    db = tmp_path/'profiles.db'
    prompt = profiles.publish_text_revision('prompt', name='Test', text='Public fixture', db_path=db)
    skill = profiles.publish_text_revision('skill', name='Test', text='Public fixture skill', db_path=db)
    fields = dict(mode='agent', name='Test', prompt_revision_id=prompt['revision_id'],
        skill_revision_id=skill['revision_id'], tools=[], rag_policy={}, db_path=db)
    old = profiles.publish_profile_revision(**fields, model_policy={'reasoning_enabled':False})
    profiles.resolve_chat_profile(session_id='existing', requested_mode='agent',
        requested_revision_id=old['revision_id'], apply_revision=True, db_path=db)
    new = profiles.publish_profile_revision(**fields,
        model_policy={'reasoning_enabled':True,'reasoning_budget_tokens':2048})
    unchanged = profiles.resolve_chat_profile(session_id='existing', requested_mode='agent', db_path=db)
    assert unchanged['model_policy']['reasoning_enabled'] is False
    assert new['model_policy']['reasoning_enabled'] is True
    with pytest.raises(ValueError, match='Бюджет'):
        profiles.publish_profile_revision(**fields, model_policy={'reasoning_budget_tokens':511})


@pytest.mark.asyncio
async def test_remote_generic_connection_rejects_unsupported_reasoning(tmp_path):
    from dataclasses import replace
    from proxy.services.model_connection_contracts import ConnectionLocality
    transport, client = _transport(tmp_path, lambda request: pytest.fail('No request allowed'))
    connection = replace(_resolved(), locality=ConnectionLocality.REMOTE)
    with pytest.raises(ModelTransportError, match='REASONING_MODE_UNSUPPORTED'):
        transport._chat_body(connection, InferenceRequest(messages=[], max_output_tokens=2048,
            reasoning_enabled=True), stream=True)
    assert 'chat_template_kwargs' not in transport._chat_body(connection,
        InferenceRequest(messages=[],max_output_tokens=128),stream=True)
    await client.aclose()
