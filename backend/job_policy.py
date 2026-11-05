"""Mandatory native job transitions; the old policy is a test-only oracle."""
import json
from backend import native_core

def normalize_patch(current, updates, now):
    updates=dict(updates)
    if 'result' in updates and not isinstance(updates['result'],str):
        updates['result']=json.dumps(updates['result'],ensure_ascii=False,separators=(',',':'),allow_nan=False)
    return native_core.required('job_patch',current=current,patch=updates,now=now)

def recovery_patch(current, reason, now):
    return native_core.required('job_recovery',current=current,reason=reason,now=now)
