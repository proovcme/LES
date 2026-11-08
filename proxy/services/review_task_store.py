"""Classic IO codec. Rust owns review transactions, revisions and quote coverage."""
from pathlib import Path
from proxy.services.document_task_store import native_request

class ReviewTaskStore:
    def __init__(self,path):
        self.path=Path(path).resolve()
        self._call('init')

    def _call(self,action,owner='',task_id='',**arguments):
        return native_request('review_'+action,dict(owner=owner,task_id=task_id,**arguments),self.path)

    def resolve_reference(self,owner,question):
        return self._call('resolve_reference',owner,question=question)

    def create(self,owner,question,selectors):
        return self._call('create',owner,question=question,selectors=selectors)

    def task(self,owner,task_id):
        return self._call('task',owner,task_id)

    def recent_reviews(self,owner):
        return self._call('recent_reviews',owner)

    def history(self,owner,task_id,offset=0):
        return self._call('history',owner,task_id,offset=offset)

    def latest_events(self,owner,task_id):
        return self._call('latest_events',owner,task_id)

    def status(self,owner,task_id,fingerprints):
        return self._call('status',owner,task_id,fingerprints=fingerprints)

    def remember_read(self,owner,task_id,unit_id,snapshot,fragment,start,stop):
        return self._call('remember_read',owner,task_id,unit_id=unit_id,snapshot=snapshot,
                          fragment=fragment,start=start,stop=stop)

    def record(self,owner,task_id,*,request_id,expected_revision,unit_id,snapshot,
               source_fingerprint,status,note,evidence,dependencies):
        return self._call('record',owner,task_id,request_id=request_id,expected_revision=expected_revision,
            unit_id=unit_id,snapshot=snapshot,source_fingerprint=source_fingerprint,status=status,note=note,
            evidence=evidence,dependencies=dependencies)
