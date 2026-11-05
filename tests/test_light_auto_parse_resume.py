"""No phantom indexing job may block chat after startup failure."""
import asyncio
import sqlite3
from types import SimpleNamespace
import pytest
from fastapi import HTTPException
from proxy.services import auto_parse_resume_service as resume
from proxy.services.job_service import JobService
from proxy.services.runtime_admission import count_active_jobs


@pytest.fixture
def state(tmp_path,monkeypatch):
    path=tmp_path/'meta.db'
    jobs=JobService(path)
    with sqlite3.connect(path) as db:
        db.execute('CREATE TABLE documents(id TEXT,status TEXT)')
        db.executemany('INSERT INTO documents VALUES (?,?)',[('one','PENDING'),('two','PENDING')])
    monkeypatch.setattr(resume,'rag_meta_db_path',lambda:str(path))
    return SimpleNamespace(job_service=jobs,job_tracker={},backend=None,current_mode={})


@pytest.mark.asyncio
async def test_stale_search_connection_creates_no_active_job(state,monkeypatch):
    async def blocked(*args,**kwargs):raise HTTPException(409,'Search connection stale')
    monkeypatch.setattr(resume,'assert_parse_admission',blocked)
    with pytest.raises(HTTPException):await resume.resume_pending_parse(state)
    assert state.job_service.list()=={} and state.job_tracker=={}
    assert count_active_jobs(state.job_service,state.job_tracker)==0
    assert resume._pending_count()==2


@pytest.mark.asyncio
async def test_failure_after_preflight_commits_terminal_job_and_releases_chat(state,monkeypatch):
    async def admitted(*args,**kwargs):pass
    async def fails(state,req,job_id):
        assert req.max_batches==2
        state.job_tracker[job_id]['status']='PARSING'
        raise HTTPException(409,'Search connection disappeared')
    monkeypatch.setattr(resume,'assert_parse_admission',admitted)
    monkeypatch.setattr(resume,'run_parse_scheduler',fails)
    task=await resume.resume_pending_parse(state);await task
    row=next(iter(state.job_service.list().values()))
    assert row['status']=='failed' and row['finished_at'] and row['processed']==0
    assert row['total']==2 and row['result']['error_code']=='AUTO_RESUME_PARSE_FAILED'
    assert state.job_tracker[row['id']]['status']=='FAILED'
    assert count_active_jobs(state.job_service,state.job_tracker)==0 and resume._pending_count()==2


@pytest.mark.asyncio
async def test_preflight_race_cannot_create_another_scheduler(state,monkeypatch):
    async def admitted(*args,**kwargs):
        state.job_tracker['other']={'type':'rag_parse_scheduler','status':'PARSING'}
    monkeypatch.setattr(resume,'assert_parse_admission',admitted)
    assert await resume.resume_pending_parse(state) is None
    assert state.job_service.list()=={} and list(state.job_tracker)==['other']


@pytest.mark.asyncio
async def test_cancelled_startup_task_is_terminal_without_losing_pending_documents(state,monkeypatch):
    async def admitted(*args,**kwargs):pass
    async def cancelled(*args,**kwargs):raise asyncio.CancelledError()
    monkeypatch.setattr(resume,'assert_parse_admission',admitted)
    monkeypatch.setattr(resume,'run_parse_scheduler',cancelled)
    task=await resume.resume_pending_parse(state)
    with pytest.raises(asyncio.CancelledError):await task
    row=next(iter(state.job_service.list().values()))
    assert row['status']=='cancelled' and row['finished_at']
    assert count_active_jobs(state.job_service,state.job_tracker)==0 and resume._pending_count()==2


@pytest.mark.asyncio
@pytest.mark.parametrize('code',['CAPABILITY_SNAPSHOT_STALE','UNKNOWN: private-internal-address'])
async def test_assigned_but_stale_search_model_is_not_reported_as_missing(monkeypatch,code):
    from proxy.services.dataset_parse_service import assert_parse_admission
    from proxy.services.model_connection_resolver_service import ModelConnectionResolutionError
    monkeypatch.setenv('LES_PRODUCT_EDITION','light')
    def stale():raise ModelConnectionResolutionError(code)
    state=SimpleNamespace(backend=SimpleNamespace(embed_parse=SimpleNamespace(
        connection_mode='active',_resolve_embedding_connection=stale)))
    with pytest.raises(HTTPException) as error:await assert_parse_admission(state)
    if code=='CAPABILITY_SNAPSHOT_STALE':
        assert 'устарела' in error.value.detail
    else:
        assert 'Подключение модели поиска не готово' in error.value.detail
        assert 'private-internal-address' not in str(error.value.detail)
