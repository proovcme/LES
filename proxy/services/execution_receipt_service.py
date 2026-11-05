"""Thin adapter to native execution receipts, independent of model prose."""
from backend import native_core


def _call(action, **arguments):
    try:
        return native_core.required('harness', request=dict(action=action, **arguments))
    except native_core.NativeCoreError as error:
        if str(error) == 'EXECUTION_COUNTS_INVALID':
            raise ValueError('Invalid execution ledger counts') from error
        raise


def ledger_receipt(kind, state):
    fields = ('total', 'pending', 'reviewed', 'needs_review', 'excluded',
              'needs_recheck', 'blocked_units', 'verification_unavailable')
    value = {key: state[key] for key in fields if key in state}
    value.update(task_id=state['task_id'], revision=state['revision'])
    return _call('ledger_receipt', kind=kind, state=value)['receipt']


def summarize(receipts):
    return _call('receipt_summary', receipts=list(receipts))['summary']


def local_file_receipt(payload):
    result = payload.get('result') if isinstance(payload, dict) else None
    result = result if isinstance(result, dict) else {}
    fields = ('path', 'persisted', 'changed', 'revision', 'history_id')
    value = dict(status=payload.get('status'), operation=payload.get('operation'),
                 result={key: result[key] for key in fields if key in result})
    return _call('local_file_receipt', payload=value)['receipt']


def execution_incomplete(trace):
    value = trace.get('execution') if isinstance(trace, dict) else None
    return isinstance(value, dict) and value.get('status') not in {'recorded', 'no_changes'}


def no_local_changes():
    return _call('no_local_changes')['summary']
