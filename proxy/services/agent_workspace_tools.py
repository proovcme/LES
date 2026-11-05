"""Existing harness adapter; binding, schema, scope and file operations belong to Rust."""
from proxy.services import agent_state_service as state

TOOL_NAMES = ('agent_workspace',)


def instructions():
    return state.call('tool_contract')['instructions']


def current_request(question, receipts):
    return state.call('tool_request', question=question, receipts=receipts)


def enabled(session_id, mode):
    if state.state_root() is None or mode not in {'agent', 'auto', 'free', 'text'} or not session_id:
        return False
    try:
        state.call('tool_binding', session_id=session_id)
        return True
    except state.AgentStateError as error:
        if error.code == 'AGENT_NOT_FOUND':
            return False
        raise


def bind(harness, session_id):
    from proxy.services.tool_contract_service import ToolContract, EffectClass, RetryPolicy, IdempotencyPolicy, ResultBudget
    from proxy.services.tool_registry_service import ToolRegistration
    from proxy.services.tool_harness_service import _result
    binding = state.call('tool_binding', session_id=session_id)
    manifest = state.call('tool_contract')

    def execute(arguments):
        try:
            result = state.call('tool_execute', session_id=session_id,
                                bound_project_id=binding['project_id'], tool_arguments=arguments)
            return _result(tool='agent_workspace', operation=arguments['operation'], inputs=[], status='ok', result=result,
                           trace='Rust-owned local workspace; persisted receipt is authoritative')
        except state.AgentStateError as error:
            return _result(tool='agent_workspace', operation=arguments['operation'], inputs=[], status='error',
                           result={'persisted': False, 'code': error.code, 'detail': str(error)},
                           warnings=[error.code], trace='No successful write receipt')

    if harness._executor.local_state_authorizer is not None:
        raise ValueError('A local workspace is already bound')
    # Grant only this server-bound tool. Rust checks scope and policy under its
    # transaction lease; models cannot authorize themselves.
    harness._executor.local_state_authorizer = lambda request: request.tool_name in TOOL_NAMES and request.actor_role in {'user', 'admin'}
    contract = ToolContract(name='agent_workspace', version='1.1.0', title='Память и рабочие файлы Леса',
        category='agent_workspace', summary=manifest['summary'], input_schema=manifest['input_schema'],
        result_schema='les_tool_result_v1', effect=EffectClass.LOCAL_STATE, scopes=('owned_workspace',), timeout_seconds=20,
        retry=RetryPolicy.NEVER, idempotency=IdempotencyPolicy.NONE, result_budget=ResultBudget(max_chars=20000,max_items=50),
        model_owned_fields=(), provenance='Native file revision/history receipt; advisory, not documentary evidence',
        tags=('execution_context_required','owned_workspace'))
    harness._registry.register(ToolRegistration(contract=contract, handler=execute))
    return TOOL_NAMES
