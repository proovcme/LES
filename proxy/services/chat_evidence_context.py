"""Compatibility codecs for the native, whole-request context owner."""
from backend import native_core
from proxy.services.context_governor_service import ContextObject, ContextPacket
from proxy.services.prompt_registry_service import build_mode_system_prompt


def _call(action, **values):
    return native_core.required('chat_context',request=dict(action=action,**values))['value']


def _objects(values):
    return tuple(ContextObject(o['object_id'],o['payload']) for o in values)


def _wire(values):
    return [dict(object_id=o.object_id,payload=o.payload,render=o.render()) for o in values]


def _context_objects(prefix,values):
    return _objects(_call('objects',prefix=prefix,values=list(values)))


def _text_context_objects(prefix,text):
    return _objects(_call('text_objects',prefix=prefix,text=text))


def workspace_memory_objects(candidates,*,registered):
    return _objects(_call('workspace_memory',registered=registered,
        candidates=[dict(kind=c.kind.value,objects=_wire(c.objects)) for c in candidates]))


def govern_inference_messages(*,preset,profile_prefix,request_payload,shortlist=(),checkpoint=(),
        working_memory=(),evidence=(),source_map=(),tool_exchange=(),dialogue=(),required_evidence=(),native_tool_exchange=()):
    result=_call('assemble',preset=dict(preset_id=preset.preset_id,input_limit=preset.input_token_limit,
        generation_reserve=preset.generation_reserve_tokens,safety_reserve=preset.safety_reserve_tokens),
        profile_prefix=profile_prefix,request_payload=request_payload,shortlist=list(shortlist),checkpoint=_wire(checkpoint),
        working_memory=_wire(working_memory),evidence=list(evidence),source_map=list(source_map),
        tool_exchange=list(tool_exchange),dialogue=list(dialogue),required_evidence=list(required_evidence),native_tool_exchange=list(native_tool_exchange))
    packet=ContextPacket.from_wire(result.get('packet',result))
    return result['messages'],packet


def context_packet_trace(packet,*,purpose):
    return _call('trace',packet=packet.to_wire(),purpose=purpose)


def prepared_context_trace(packet,*,purpose,source_map,evidence):
    return _call('prepared_trace',packet=packet.to_wire(),purpose=purpose,source_map=list(source_map),evidence=list(evidence))


def confirm_context_delivery(trace,usage):
    trace.update(_call('confirm',trace=trace,usage=usage))


def profile_temperature(profile_snapshot,*,fallback):
    return _call('temperature',profile=profile_snapshot,fallback=fallback)


def profile_research_rounds(profile_snapshot,*,configured):
    return _call('rounds',profile=profile_snapshot,configured=configured)


def _bounded_source_blocks(text,*,max_chars=700):
    return _call('bounded_blocks',text=text,max_chars=max_chars)


def selector_evidence_payload(*,attachment_context,rendered_context):
    return _call('selector_evidence',attachment_context=attachment_context,rendered_context=rendered_context)


def source_context_blocks(text):
    return _call('source_blocks',text=text)


def model_visible_source_map(packet,source_map):
    return _call('visible',packet=packet.to_wire(),source_map=list(source_map))


def selector_context_shortlist(shortlist,*,native_tool_schemas):
    return tuple(_call('selector_shortlist',shortlist=list(shortlist),native_tool_schemas=native_tool_schemas))


def initial_selector_context(rendered_context,*,model_authored_initial_query):
    return _call('selector_initial',rendered_context=rendered_context,model_authored_initial_query=model_authored_initial_query)


def profile_system_prompt(profile_snapshot,*,strict):
    # Registry snapshot resolution is still owned by the remaining chat adapter.
    try:
        return _call('profile_prompt',profile=profile_snapshot)
    except native_core.NativeCoreError as error:
        if str(error)!='CHAT_PROFILE_DEFAULT_REQUIRED':
            raise
    return _call('profile_prompt',profile=profile_snapshot,default_prompt=build_mode_system_prompt('rag'))


def tool_decision_profile(profile_prompt,*,native):
    return _call('tool_profile',profile_prompt=profile_prompt,native=native)
