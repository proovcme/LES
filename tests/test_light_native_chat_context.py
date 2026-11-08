"""Complete native context assembly, source visibility and delivery diagnostics."""
import hashlib
import os
import pytest
from backend import native_core

pytestmark=pytest.mark.skipif(not os.getenv('LES_CORE_LIBRARY'),reason='Build native core')


def call(action,**values):
    return native_core.required('chat_context',request=dict(action=action,**values))['value']


def request(**values):
    return dict(preset=dict(preset_id='context-test',input_limit=1800,generation_reserve=200,safety_reserve=100),
        profile_prefix='private-profile',request_payload='private-question',**values)


def test_native_context_drops_whole_large_source_and_its_reference():
    text='[Источник 2] 日本語 🌲 Проверяемая цитата'
    sources=[dict(index=1,doc_name='Large.pdf'),dict(index=2,doc_name='Small.pdf',page=2)]
    result=call('assemble',**request(evidence=['[Источник 1]\n'+'x'*10000,text],source_map=sources))
    packet=result['packet']
    assert packet['included_tokens']<=packet['input_budget_tokens']
    assert call('visible',packet=packet,source_map=sources)==sources[1:]
    assert 'Large.pdf' not in str(result['messages'])
    trace=call('prepared_trace',packet=packet,purpose='answer',source_map=sources,evidence=[text,'[Источник 1]'])
    assert trace['omitted_sources']==[dict(index=1,doc_name='Large.pdf')]
    assert trace['delivery']=='prepared' and trace['sent_to_model'] is False
    evidence=next(s for s in trace['sections'] if s['kind']=='evidence')['objects'][0]
    assert evidence['text']==text and evidence['sha256']==hashlib.sha256(text.encode()).hexdigest()
    assert 'private-profile' not in str(trace) and 'private-question' not in str(trace)
    confirmed=call('confirm',trace=trace,usage={'prompt_tokens':True})
    assert confirmed['sent_to_model'] is True and 'provider_prompt_tokens' not in confirmed
    assert trace['sent_to_model'] is False


def test_current_tool_exchange_is_whole_and_overflow_is_explicit():
    pair=[dict(role='assistant',content='',tool_calls=[dict(id='c1',type='function',function=dict(name='read_source',arguments='{}'))]),
        dict(role='tool',tool_call_id='c1',content='x'*9000)]
    result=call('assemble',**request(native_tool_exchange=[pair]))
    assert result['overflow'] is True
    assert 'current-tool-turn:0' in result['object_ids']
    assert 'messages' not in result


def test_coverage_deduplicates_points_and_preserves_conflicting_counts_as_unknown():
    a=dict(index=1,dataset_id='a',doc_name='Same.pdf',parent_id='parent',qdrant_point_id='p1',section_fragment_count=2)
    b=dict(a,index=2,qdrant_point_id='p2',section_fragment_count=3)
    c=dict(a,index=3,dataset_id='b')
    report=call('section_coverage',sources=[a,b,c],visible=[a,a,c])
    first,other=report['sections']
    assert first['model_fragments']==1 and first['status']=='unknown' and first['omitted_fragments'] is None
    assert other['dataset_id']=='b' and other['status']=='partial' and other['omitted_fragments']==1
    assert 'answerability are not established' in report['scope']


def test_memory_and_dialogue_never_make_a_source_model_visible():
    sources=[dict(index=1,doc_name='Unseen.pdf')]
    result=call('assemble',**request(source_map=sources,
        working_memory=[dict(object_id='agent-file:memory',payload='[Источник 1] advisory only')],
        dialogue=[dict(role='assistant',content='[Источник 1] old reply')]))
    assert call('visible',packet=result['packet'],source_map=sources)==[]


def test_explicit_profile_does_not_read_default_prompt_registry(monkeypatch):
    from proxy.services import chat_evidence_context as bridge
    def forbidden(*args):raise AssertionError('Default registry must not replace a bound profile')
    monkeypatch.setattr(bridge,'build_mode_system_prompt',forbidden)
    assert bridge.profile_system_prompt(dict(prompt_text='Мой стиль',skill_text='Мой навык'),strict=False).startswith('Мой стиль')


def test_numeric_rendering_and_unicode_line_boundaries_preserve_old_contract():
    obj=call('objects',prefix='value',values=[dict(ratio=1e-5,zero=-0.0,label='🌲')])[0]
    assert obj['render']=='{"label":"🌲","ratio":1e-05,"zero":-0.0}'
    assert call('bounded_blocks',text='ab\x85cd\u2028日本語\r\n🌲',max_chars=4)==['ab','cd','日本語','🌲']
    assert call('temperature',profile={'model_policy':{'temperature':False}},fallback=.6)==0
