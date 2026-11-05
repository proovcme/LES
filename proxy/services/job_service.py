"""Compatibility codec; durable job transactions belong exclusively to Rust."""
from datetime import datetime
import json
from pathlib import Path
import uuid

from proxy.config import META_DB_PATH
from backend.workspace_native import catalog_call


def _decode(row):
    if row:
        try:
            row['result'] = json.loads(row.get('result') or '{}')
        except json.JSONDecodeError:
            row['result'] = {}
    return row


class JobService:
    def __init__(self, db_path=META_DB_PATH):
        self.db_path = Path(db_path)
        self.init_db()

    def _call(self, action, **arguments):
        return catalog_call(self.db_path, 'job_' + action, **arguments)

    def init_db(self):
        self._call('init')

    def create(self, job_type, source='', dataset_id='', dataset_name='', total=0,
               status='queued', message=''):
        return _decode(self._call(
            'create', id=str(uuid.uuid4())[:12], type=job_type,
            now=datetime.now().isoformat(),
            patch=dict(status=status, source=source, dataset_id=dataset_id,
                       dataset_name=dataset_name, total=total, message=message)))

    def update(self, job_id, **updates):
        if not updates:
            return self.get(job_id) or {}
        if 'result' in updates and not isinstance(updates['result'], str):
            updates['result'] = json.dumps(updates['result'], ensure_ascii=False,
                                           separators=(',', ':'), allow_nan=False)
        return _decode(self._call('update', id=job_id, patch=updates,
                                  now=datetime.now().isoformat()))

    def get(self, job_id):
        return _decode(self._call('get', id=job_id))

    def list(self, limit=200):
        return {key: _decode(row) for key, row in self._call('list', limit=limit).items()}

    def list_active_ids(self, limit=500):
        return self._call('active', limit=limit)

    def mark_interrupted_active_jobs(self, reason):
        return self._call('recover', reason=reason, now=datetime.now().isoformat())
