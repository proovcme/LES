"""Real native HTTP, SQLite, provider and crash boundaries; no application Python server."""
from concurrent.futures import ThreadPoolExecutor
import time
import sqlite3
import pytest
from tests.test_light_native_chat_turn import native_turn, HEADERS, PATH
from tests.test_light_native_api import wait_for

PRIVATE='/api/light/internal-context'

def seed(t, count=8):
    sid=t.client.post('/api/workspace/sessions',json={'title':'Memory 日本語'}).raise_for_status().json()['session_id']
    with sqlite3.connect(t.database) as db:
        db.execute("CREATE TABLE chat_history(id INTEGER PRIMARY KEY,timestamp TEXT DEFAULT CURRENT_TIMESTAMP,session_id TEXT,question TEXT,answer TEXT,sources TEXT,crag_status TEXT DEFAULT 'UNVALIDATED',latency_sec REAL,tokens INTEGER,success INTEGER)")
        db.executemany('INSERT INTO chat_history(id,session_id,question,answer,success) VALUES(?,?,?,?,1)',[(i,sid,f'План {i}: проверить библиотеку 日本語',f'Обсудили {i}') for i in range(1,count+1)])
    return sid,f'/api/workspace/memory/context/{sid}'

def context(t,url):return t.client.get(url).raise_for_status().json()
def job(t,sid):return t.client.post(PRIVATE,json={'action':'status','session_id':sid},headers=HEADERS).raise_for_status().json()

def test_manual_summary_preserves_exact_utterances_and_history(native_turn):
    t=native_turn;sid,url=seed(t)
    assert t.client.post(PRIVATE,json={'action':'summarize','session_id':sid}).status_code==404
    assert t.client.post(url+'/summarize',json={}).status_code==422
    t.scripts.append({'text':'{"keep":[0,2]}'})
    response=t.client.post(url+'/summarize',json={'expected_revision':0});assert response.status_code==200,response.text
    state=response.json()
    assert state['summary']=='Пользователь: План 1: проверить библиотеку 日本語\n\nПользователь: План 2: проверить библиотеку 日本語'
    assert state['through_id']==8 and state['revision']==1
    assert t.client.post(url+'/summarize',json={'expected_revision':0}).status_code==409
    assert len(t.seen)==1
    assert context(t,url)['summarized_turns']==8
    with sqlite3.connect(t.database) as db:assert db.execute('SELECT count(*) FROM chat_history').fetchone()==(8,)

def test_auto_summary_starts_from_oldest_turn_and_keeps_recent(native_turn):
    t=native_turn;sid,url=seed(t,170)
    # The first batch cannot jump to the latest history window.
    t.scripts.extend([{'text':'{"keep":[0]}'},{'text':'{"keep":[0]}'},{'text':'{"keep":[0]}'},{'text':'{"keep":[0]}'}])
    t.client.post(PRIVATE,json={'action':'enqueue','session_id':sid},headers=HEADERS).raise_for_status()
    wait_for(lambda:job(t,sid)['status']=='completed',timeout=30)
    state=context(t,url)
    assert 164<=state['through_id']<=168 and state['summary']=='Пользователь: План 1: проверить библиотеку 日本語'
    assert 'План 1:' in t.seen[0]['messages'][1]['content']
    assert all('План 170:' not in r['messages'][1]['content'] for r in t.seen)
    assert state['remembered_turns']==170

def test_summary_never_rewrites_memory_after_concurrent_forget(native_turn):
    t=native_turn;sid,url=seed(t)
    t.scripts.append({'text':'{"keep":[0]}','wait':True,'complete_after_wait':True})
    with ThreadPoolExecutor(max_workers=1) as pool:
        future=pool.submit(t.client.post,url+'/summarize',json={'expected_revision':0})
        assert t.blocked.wait(5)
        forgotten=t.client.post(url+'/forget',json={'expected_revision':0}).raise_for_status().json()
        t.release.set();assert future.result(10).status_code==409
    assert forgotten['cutoff_id']==8 and context(t,url)['summary']==''

def test_invalid_summary_and_disabled_memory_never_hide_the_failure(native_turn):
    t=native_turn;sid,url=seed(t)
    t.client.patch(url,json={'expected_revision':0,'summary':'Сохранено пользователем'}).raise_for_status()
    t.scripts.append({'text':'Проверка уже выполнена, всё хорошо.'})
    state=t.client.post(url+'/summarize',json={'expected_revision':1}).raise_for_status().json()
    assert state['summary']=='Сохранено пользователем' and state['through_id']==0
    assert 'История сохранена' in state['last_error']
    t.client.patch(url,json={'expected_revision':state['revision'],'enabled':False}).raise_for_status()
    disabled=context(t,url)
    t.client.post(url+'/summarize',json={'expected_revision':disabled['revision']}).raise_for_status()
    assert len(t.seen)==1

def test_foreground_preempts_idle_model_and_requeues_without_losing_memory(native_turn):
    t=native_turn;sid,url=seed(t)
    t.scripts.extend([{'text':'{"keep":[0]}','wait':True},{'text':'Новый ответ'},{'text':'{"keep":[0]}'}])
    t.client.post(PRIVATE,json={'action':'enqueue','session_id':sid},headers=HEADERS).raise_for_status()
    assert t.blocked.wait(8)
    response=t.client.post(PATH,json={'question':'Новое сообщение'},headers=HEADERS).raise_for_status().json()
    assert response['answer']=='Новый ответ'
    assert context(t,url)['summary']==''
    t.release.set()
    wait_for(lambda:job(t,sid)['status']=='completed',timeout=15)
    assert context(t,url)['through_id']==6 and job(t,sid)['attempts']==2

def test_summary_queue_resumes_after_hard_process_stop(native_turn):
    t=native_turn;sid,url=seed(t)
    t.scripts.extend([{'text':'{"keep":[0]}','wait':True},{'text':'{"keep":[0]}'}])
    t.client.post(PRIVATE,json={'action':'enqueue','session_id':sid},headers=HEADERS).raise_for_status()
    assert t.blocked.wait(8)
    t.children[-1].kill();t.children[-1].wait(5);t.release.set()
    # Recovery happens at startup, without a new summarize/chat request.
    t.env['LES_STARTUP_BACKGROUND_MUTATIONS']='true';t.start()
    wait_for(lambda:context(t,url)['through_id']==6,timeout=15)
    assert job(t,sid)['attempts']==2 and len(t.seen)==2
    with sqlite3.connect(t.database) as db:assert db.execute('SELECT count(*) FROM chat_history').fetchone()==(8,)

def test_manual_summary_waits_for_existing_foreground_slot(native_turn):
    t=native_turn;sid,url=seed(t)
    lease=t.client.post(PRIVATE,json={'action':'foreground_begin'},headers=HEADERS).raise_for_status().json()
    t.scripts.append({'text':'{"keep":[0]}'})
    try:
        with ThreadPoolExecutor(max_workers=1) as pool:
            future=pool.submit(t.client.post,url+'/summarize',json={'expected_revision':0})
            time.sleep(.2)
            assert not future.done() and not t.seen
            t.client.post(PRIVATE,json={'action':'foreground_end','lease':lease['lease']},headers=HEADERS).raise_for_status()
            assert future.result(10).raise_for_status().json()['through_id']==8
    finally:
        t.client.post(PRIVATE,json={'action':'foreground_end','lease':lease['lease']},headers=HEADERS)

def test_background_failure_keeps_its_exact_code_and_previous_memory(native_turn):
    t=native_turn;sid,url=seed(t)
    t.client.patch(url,json={'expected_revision':0,'summary':'Сохранено пользователем'}).raise_for_status()
    t.scripts.append({'text':'{"keep":[true]}'})
    t.client.post(PRIVATE,json={'action':'enqueue','session_id':sid},headers=HEADERS).raise_for_status()
    wait_for(lambda:job(t,sid)['status']=='failed',timeout=10)
    assert job(t,sid)['last_code']=='SUMMARY_SELECTION_INVALID'
    state=context(t,url)
    assert state['summary']=='Сохранено пользователем' and state['through_id']==0
    assert 'История сохранена' in state['last_error'] and len(t.seen)==1

@pytest.mark.asyncio
async def test_classic_codec_uses_the_same_native_owner_and_conflict(native_turn,monkeypatch):
    from proxy.services import local_model_http, conversation_context_service, background_summary_service
    t=native_turn;sid,url=seed(t)
    monkeypatch.setattr(local_model_http,'_bridge',(str(t.client.base_url).rstrip('/')+'/api/light/internal-model-http',HEADERS['X-Les-Model-Token']))
    async with background_summary_service.foreground_request(sid):
        assert not t.seen
    t.scripts.append({'text':'{"keep":[0]}'})
    state=await conversation_context_service.summarize(sid,force=True,expected_revision=0)
    assert state['through_id']==8 and len(t.seen)==1
    with pytest.raises(conversation_context_service.ContextConflict):
        await conversation_context_service.summarize(sid,force=True,expected_revision=0)
    assert context(t,url)['summary']==state['summary']

def test_completed_native_turn_durably_schedules_memory_without_an_enqueue_call(native_turn):
    t=native_turn;sid,url=seed(t,6)
    t.scripts.extend([{'text':'Продолжим разговор.'},{'text':'{"keep":[0]}'}])
    t.client.post(PATH,json={'question':'Продолжим','session_id':sid},headers=HEADERS).raise_for_status()
    wait_for(lambda:context(t,url)['through_id']==5,timeout=12)
    assert job(t,sid)['status']=='completed' and len(t.seen)==2
    with sqlite3.connect(t.database) as db:assert db.execute('SELECT count(*) FROM chat_history').fetchone()==(7,)

def test_outbox_failure_cannot_commit_a_successful_answer_or_lose_its_cause(native_turn):
    t=native_turn;sid,url=seed(t,6)
    job(t,sid)  # Opens the shared owner/schema without enqueuing any work.
    with sqlite3.connect(t.database) as db:
        db.execute("CREATE TRIGGER fail_summary_outbox BEFORE INSERT ON les_summary_jobs BEGIN SELECT RAISE(ABORT,'fixture outbox failure'); END")
    response=t.client.post(PATH,json={'question':'Продолжим','session_id':sid},headers=HEADERS)
    assert response.status_code==503 and response.json()['code']=='CHAT_STORAGE_UNAVAILABLE'
    with sqlite3.connect(t.database) as db:
        row=db.execute('SELECT answer,crag_status,success,artifact_json FROM chat_history ORDER BY id DESC LIMIT 1').fetchone()
        assert row[:3]==('Ответ 日本語','FAILED',0) and 'CHAT_STORAGE_UNAVAILABLE' in row[3]
        assert db.execute('SELECT status FROM les_chat_runs').fetchone()==('failed',)
        assert db.execute('SELECT count(*) FROM les_summary_jobs').fetchone()==(0,)

def test_remote_foreground_calls_keep_their_parallel_pool_without_network(native_turn):
    from proxy.services.model_connection_contracts import ConnectionLocality,ConnectionRole
    t=native_turn
    remote=t.registry.create_connection(display_name='Remote fixture',base_url='https://model.example.invalid/v1',model_id='remote-fixture',
        locality=ConnectionLocality.REMOTE,requested_context_tokens=24000,secret_ref=None,extension_type=None,actor='admin:test')
    current=t.registry.get_role_binding(ConnectionRole.ANSWER)
    t.registry.bind_role(ConnectionRole.ANSWER,remote.revision_id,expected_binding_revision=current.binding_revision,actor='admin:test')
    leases=[]
    try:
        for _ in range(4):
            leases.append(t.client.post(PRIVATE,json={'action':'foreground_begin'},headers=HEADERS).raise_for_status().json()['lease'])
        assert not t.seen
    finally:
        for lease in leases:t.client.post(PRIVATE,json={'action':'foreground_end','lease':lease},headers=HEADERS).raise_for_status()

def test_oversized_legacy_turn_is_not_partly_summarized_or_sent_to_model(native_turn):
    t=native_turn;sid,url=seed(t)
    with sqlite3.connect(t.database) as db:db.execute('UPDATE chat_history SET answer=? WHERE id=1',('日本語 '*100000,))
    state=t.client.post(url+'/summarize',json={'expected_revision':0}).raise_for_status().json()
    assert state['summary']=='' and state['through_id']==0 and 'слишком длинное' in state['last_error']
    assert not t.seen
    with sqlite3.connect(t.database) as db:assert db.execute('SELECT length(answer) FROM chat_history WHERE id=1').fetchone()==(400000,)
