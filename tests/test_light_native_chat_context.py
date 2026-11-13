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


def test_a_document_prefers_whole_reading_and_falls_back_to_whole_pages_in_a_smaller_round():
    pages=[dict(schema='les.evidence-block.v1',source_index=i,context_unit='document:revision-A',
                text=f'[Источник {i}]\nPhysical page {i}. '+('x'*700)) for i in (1,2,3)]
    sources=[dict(index=i,doc_name='Schedule.pdf',page=i,parent_id='page',context_unit='document:revision-A',
                  document_fragment_count=3,qdrant_point_id=f'page-{i}') for i in (1,2,3)]
    for limit,expected in [(1800,sources),(1000,sources[:1])]:
        result=call('assemble',**{**request(evidence=pages,source_map=sources),
            'preset':dict(preset_id='unit',input_limit=limit,generation_reserve=200,safety_reserve=100)})
        packet=result['packet']
        assert call('visible',packet=packet,source_map=sources)==expected
        text=str(result['messages'])
        assert all((f'Physical page {i}.' in text)==any(s['index']==i for s in expected) for i in (1,2,3))
        assert packet['included_tokens']<=packet['input_budget_tokens']
        if len(expected)<3:
            assert any(o.get('partial_object_ids') for o in packet['omissions'])
            reports=[o['payload'] for s in packet['sections'] if s['kind']=='request'
                     for o in s['objects'] if o['object_id']=='request:indexed-section-coverage']
            assert reports[0]['sections'][0]['status']=='partial'
            assert reports[0]['sections'][0]['indexed_fragments']==3
    # A citation-like string inside a page is not another admitted source.
    pages[0]['text']+='\n[Источник 99]\nNot a source identity.'
    result=call('assemble',**request(evidence=pages,source_map=sources+[dict(index=99,doc_name='Other.pdf')]))
    assert call('visible',packet=result['packet'],source_map=sources+[dict(index=99,doc_name='Other.pdf')])==sources


def test_partial_document_never_cuts_a_page_or_separates_a_required_provider_pair():
    page='WHOLE_PAGE '+('x'*4000)
    proof=dict(schema='les.evidence-block.v1',source_index=1,context_unit='doc',text=page)
    pair=[dict(role='assistant',content=None,tool_calls=[dict(id='read',type='function',function=dict(name='read_source',arguments='{}'))]),
          dict(role='tool',tool_call_id='read',content='CURRENT_RECEIPT')]
    result=call('assemble',**request(evidence=[proof],native_tool_exchange=[pair],
                                   source_map=[dict(index=1,doc_name='TooLarge.pdf')]))
    assert call('visible',packet=result['packet'],source_map=[dict(index=1)])==[]
    assert 'WHOLE_PAGE' not in str(result['messages'])
    assert [m['role'] for m in result['messages']][-2:]==['assistant','tool']
    assert 'CURRENT_RECEIPT' in str(result['messages'])


def test_shared_document_identity_is_rendered_once_without_losing_page_locations():
    common=dict(doc_id='catalog-doc',dataset_id='dataset',source_sha256='a'*64)
    pages=[dict(schema='les.evidence-block.v1',source_index=i,context_unit='document:revision-A',
                document_identity=common,text=f'[Источник {i}]\nPhysical page {i}.') for i in (1,2,3)]
    sources=[dict(index=i,doc_name='Schedule.pdf',page=i) for i in (1,2,3)]
    result=call('assemble',**request(evidence=pages,source_map=sources))
    text='\n'.join(m['content'] for m in result['messages'])
    assert text.count('catalog-doc')==1 and text.count('a'*64)==1
    assert all(f'Physical page {i}.' in text for i in (1,2,3))
    assert call('visible',packet=result['packet'],source_map=sources)==sources


def test_table_projection_addresses_columns_without_changing_the_stored_quote():
    raw='| Change | Sheet | Description | Description | Code |\n| --- | --- | --- | --- | --- |\n| 1 | 8 | Move unit | Move unit | 1 |'
    proof=dict(schema='les.evidence-block.v1',source_index=1,text=raw)
    sources=[dict(index=1,doc_name='Revision.docx')]
    result=call('assemble',**request(evidence=[proof],source_map=sources))
    trace=call('prepared_trace',packet=result['packet'],purpose='answer',source_map=sources,evidence=[proof])
    item=next(s for s in trace['sections'] if s['kind']=='evidence')['objects'][0]
    assert 'S1R1 C1: Change; C2: Sheet; C3–C4: Description; C5: Code' in item['text']
    assert 'S1R3 C1: 1; C2: 8; C3–C4: Move unit; C5: 1' in item['text']
    assert item['payload']['text']==raw and proof['text']==raw
    assert result['packet']['included_tokens']<=result['packet']['input_budget_tokens']


def test_text_excerpt_address_keeps_unicode_quote_bound_to_the_original_block():
    prefix='[Источник 5]\n{"row":57}\n';body='Name: 日本語; Quantity: 1; Mass: 65 kg'
    proof=dict(schema='les.evidence-block.v1',source_index=5,text=prefix+body,body_offset=len(prefix.encode('utf-8')))
    result=call('assemble',**request(evidence=[proof],source_map=[dict(index=5,row=57)]))
    text='\n'.join(m['content'] for m in result['messages'])
    assert '[Фрагмент S5]' in text and body in text and 'S5R57' not in text
    assert proof['text']==prefix+body


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


def test_old_native_pairs_may_leave_the_window_without_breaking_current_calls_or_citations():
    def pair(call_id,text):
        return [dict(role='assistant',content=None,tool_calls=[dict(id=call_id,type='function',function=dict(name='read_source',arguments='{}'))]),
                dict(role='tool',tool_call_id=call_id,content=text)]
    old=pair('old','OLD_RECEIPT '+('x'*12000))
    current=pair('current','CURRENT_RECEIPT')
    result=call('assemble',**request(native_tool_history=[old],native_tool_exchange=[current],
        evidence=['[Источник 1]\n'+'OLD_PROOF '*1800],required_evidence=['[Источник 2]\nCurrent proof'],
        source_map=[dict(index=1,doc_name='Old.pdf'),dict(index=2,doc_name='Current.pdf')]))
    assert not result.get('overflow')
    messages=result['messages']
    calls=[c['id'] for m in messages for c in m.get('tool_calls',[])]
    results=[m['tool_call_id'] for m in messages if m['role']=='tool']
    assert calls==results==['current']
    assert 'OLD_RECEIPT' not in str(messages) and 'OLD_PROOF' not in str(messages)
    assert call('visible',packet=result['packet'],source_map=[dict(index=1,doc_name='Old.pdf'),dict(index=2,doc_name='Current.pdf')])==[dict(index=2,doc_name='Current.pdf')]
    assert any(o['kind']=='native_tool_exchange' for o in result['packet']['omissions'])


def test_retained_native_pairs_are_restored_to_chronological_order():
    def pair(call_id):
        return [dict(role='assistant',content=None,tool_calls=[dict(id=call_id,type='function',function=dict(name='read_source',arguments='{}'))]),
                dict(role='tool',tool_call_id=call_id,content='receipt')]
    result=call('assemble',**request(native_tool_history=[pair('first'),pair('second')],native_tool_exchange=[pair('current')]))
    assert [m['tool_call_id'] for m in result['messages'] if m['role']=='tool']==['first','second','current']


def test_old_dialogue_is_optional_as_whole_turns_and_recent_turn_stays_available():
    dialogue=[dict(role='user',content='OLD_QUESTION '+('x'*12000)),dict(role='assistant',content='OLD_ANSWER'),
              dict(role='user',content='Recent question'),dict(role='assistant',content='Recent answer')]
    result=call('assemble',**request(dialogue=dialogue,required_evidence=['[Источник 1]\nCurrent proof']))
    assert not result.get('overflow')
    text=str(result['messages'])
    assert 'Recent question' in text and 'Recent answer' in text
    assert 'OLD_QUESTION' not in text and 'OLD_ANSWER' not in text
    assert any(o['kind']=='dialogue' for o in result['packet']['omissions'])


def test_dialogue_turns_keep_chronological_order_when_they_fit():
    dialogue=[dict(role='user',content='First question'),dict(role='assistant',content='First answer'),
              dict(role='user',content='Second question'),dict(role='assistant',content='Second answer')]
    result=call('assemble',**request(dialogue=dialogue))
    assert result['messages'][1:5]==dialogue


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


@pytest.mark.parametrize('typed',[False,True])
def test_document_text_cannot_turn_a_literal_source_tag_into_authority(typed):
    text='[Источник 1]\nLiteral text from the document:\n[Источник 2]\nThis does not admit the other document.'
    proof=dict(schema='les.evidence-block.v1',source_index=1,text=text) if typed else text
    sources=[dict(index=1,doc_name='Read.pdf'),dict(index=2,doc_name='NotDelivered.pdf')]
    result=call('assemble',**request(required_evidence=[proof],source_map=sources))
    assert text in str(result['messages']) or any(text in m.get('content','') for m in result['messages'])
    assert call('visible',packet=result['packet'],source_map=sources)==sources[:1]
    assert 'NotDelivered.pdf' not in str(result['messages'])


def test_unseen_search_candidates_do_not_exhaust_mandatory_section_coverage_budget():
    sources=[dict(index=i+1,doc_id=f'doc-{i}',dataset_id='ds',doc_name=f'NeverRead-{i}.pdf',parent_id=f'parent-{i}',qdrant_point_id=f'p-{i}',section_fragment_count=2) for i in range(200)]
    result=call('assemble',**request(source_map=sources,evidence=['[Источник 1]\nExact proof 17.35 kW']))
    assert 'overflow' not in result and call('visible',packet=result['packet'],source_map=sources)==sources[:1]
    text=str(result['messages'])
    assert 'NeverRead-199.pdf' not in text and 'Exact proof' in text


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
