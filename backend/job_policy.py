"""Native job policy and its compatible Python rollback implementation."""
import json
from backend import native_core

ACTIVE = frozenset({'queued', 'parsing', 'running'})
TERMINAL = frozenset({'completed', 'failed', 'cancelled'})
TEXT_FIELDS = frozenset({'source', 'dataset_id', 'dataset_name', 'message'})
COUNTERS = frozenset({'total', 'processed', 'errors'})


def _decision(result):
    if (not isinstance(result, dict) or type(result.get('ignored')) is not bool
            or not isinstance(result.get('patch'), dict)
            or set(result['patch']) - TEXT_FIELDS - COUNTERS - {'status','result','updated_at','finished_at'}):
        raise native_core.NativeCoreError('CORE_JOB_RESPONSE_INVALID')
    return result


def _reject_constant(_):
    raise ValueError('CORE_JOB_RESULT_INVALID')


def normalize_patch(current, updates, now):
    updates = dict(updates)
    if 'result' in updates and not isinstance(updates['result'], str):
        updates['result'] = json.dumps(updates['result'], ensure_ascii=False, separators=(',', ':'), allow_nan=False)
    native = native_core.call('job_patch', current=current, patch=updates, now=now)
    if native is not None:
        return _decision(native)
    patch = dict(updates)
    for field, value in patch.items():
        if field in COUNTERS:
            if type(value) is not int or not 0 <= value <= 2**63-1:
                raise ValueError('CORE_JOB_COUNTER_INVALID')
        elif field in TEXT_FIELDS:
            if not isinstance(value, str):
                raise ValueError('CORE_JOB_FIELD_INVALID')
        elif field == 'status':
            if not isinstance(value, str) or value.lower() not in ACTIVE | TERMINAL:
                raise ValueError('CORE_JOB_STATUS_INVALID')
            patch[field] = value.lower()
        elif field == 'result':
            if isinstance(value, str):
                try:
                    json.loads(value, parse_constant=_reject_constant)
                except ValueError as error:
                    raise ValueError('CORE_JOB_RESULT_INVALID') from error
            else:
                patch[field] = json.dumps(value, ensure_ascii=False, separators=(',', ':'), allow_nan=False)
        else:
            raise ValueError('CORE_JOB_UPDATE_FIELD_INVALID')
    previous = str(current.get('status') or '').lower()
    if ((current.get('finished_at') or previous in TERMINAL)
            and ('status' in patch and patch['status'] != previous or COUNTERS.intersection(patch))):
        return {'ignored': True, 'patch': {}}
    patch['updated_at'] = now
    if patch.get('status') in TERMINAL:
        patch['finished_at'] = current.get('finished_at') or now
    return {'ignored': False, 'patch': patch}


def recovery_patch(current, reason, now):
    native = native_core.call('job_recovery', current=current, reason=reason, now=now)
    if native is not None:
        return _decision(native)
    if current.get('finished_at') or str(current.get('status') or '').lower() not in ACTIVE:
        return {'ignored': True, 'patch': {}}
    message = f'Interrupted by {reason}'
    if current.get('message'):
        message = current['message'] + ' | ' + message
    return normalize_patch(current, {'status': 'cancelled', 'message': message}, now)
