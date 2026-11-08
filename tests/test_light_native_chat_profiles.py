"""Native-only profile and tool contracts survive edits, archival and process restart."""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
from uuid import uuid4

import httpx
import pytest
from test_light_native_api import free_port, wait_for


@pytest.fixture
def profiles(tmp_path):
    executable=os.getenv('LES_TEST_NATIVE_API')
    if not executable:
        pytest.skip('Build les-api and set LES_TEST_NATIVE_API')
    port,upstream=free_port(),free_port()
    while port==upstream:
        upstream=free_port()
    state=tmp_path/'Состояние 日本語';state.mkdir()
    database=state/'meta.db'
    policy=state/'policy.json'
    policy.write_text(json.dumps({'schema':'les.native-auth-policy.v1','compatible':True,
        'trusted_role':'admin','cors_origins':[],'cors_loopback':True,'proxy_header':'x-les-trusted-network'}),encoding='utf-8')
    command=[executable,'--listen',f'127.0.0.1:{port}','--upstream',f'http://127.0.0.1:{upstream}',
        '--state',str(state),'--database',str(database),'--auth-policy-file',str(policy)]
    children=[]
    log=(state/'api.log').open('wb')
    client=httpx.Client(base_url=f'http://127.0.0.1:{port}',trust_env=False,timeout=10)
    def start():
        child=subprocess.Popen(command,stdout=log,stderr=subprocess.STDOUT,
            creationflags=getattr(subprocess,'CREATE_NO_WINDOW',0))
        children.append(child)
        wait_for(lambda:client.get('/api/profiles').status_code==200)
        return child
    def restart():
        children[-1].kill();children[-1].wait(timeout=5)
        return start()
    try:
        start()
        yield client,database,restart
    finally:
        client.close()
        for child in children:
            if child.poll() is None:
                child.terminate()
            child.wait(timeout=5)
        log.close()


def test_full_profile_path_preserves_bound_instructions_after_library_deletion_and_restart(profiles):
    client,database,restart=profiles
    catalog=client.get('/api/profiles').raise_for_status()
    assert catalog.headers['x-les-handler']=='rust.profiles'
    assert catalog.json()['default_mode']=='agent'
    assert {row['mode'] for row in catalog.json()['profiles']}=={'agent','search'}
    def text(kind,name,value,**fields):
        return client.post('/api/profiles/text-revisions',json={'kind':kind,'name':name,'text':value,**fields}).raise_for_status().json()
    prompt=text('prompt','Редактор','\x1c\u00a0Пиши ясно 日本語.\u00a0\x1f')
    first=text('skill','Формат','Сохраняй таблицы.')
    second=text('skill','Стиль','Короткие предложения.')
    assert prompt['text']=='Пиши ясно 日本語.'
    body={'mode':'agent','name':'Профиль редактора','prompt_revision_id':prompt['revision_id'],
        'skill_revision_id':first['revision_id'],'additional_skill_revision_ids':[second['revision_id']],
        'tools':['web_search'],'model_policy':{},'rag_policy':{}}
    version=client.post('/api/profiles/revisions',json=body).raise_for_status().json()
    sid=str(uuid4())
    binding=client.put(f'/api/profiles/chats/{sid}/binding',json={'mode':'agent','profile_revision_id':version['revision_id']}).raise_for_status().json()
    assert binding['tools']==['web_search','web_read']
    assert binding['effective_capability_bridges']==['paired_web_capability_v1']
    instructions='/api/profiles/revisions/'+version['revision_id']+'/instructions'
    before=client.get(instructions).raise_for_status().json()
    for value in (prompt['text'],first['text'],second['text']):
        assert before['text'].count(value)==1
    assert client.delete('/api/profiles/skill/'+second['revision_id']).status_code==200
    assert text('skill','Архивная копия',second['text'],source_revision_id=second['revision_id'])['source_revision_id']==second['revision_id']
    client.post('/api/profiles/agent/activate/'+version['revision_id']).raise_for_status()
    assert client.delete('/api/profiles/profile/'+version['revision_id']).status_code==409
    with sqlite3.connect(database) as db:
        frozen=db.execute('SELECT snapshot_json FROM les_chat_profile_bindings WHERE session_id=?',[sid]).fetchone()[0]
    restart()
    assert client.get(instructions).raise_for_status().json()==before
    active=next(v for v in client.get('/api/profiles').json()['profiles'] if v['mode']=='agent')['active']
    assert active['revision_id']==version['revision_id']
    with sqlite3.connect(database) as db:
        assert db.execute('SELECT snapshot_json FROM les_chat_profile_bindings WHERE session_id=?',[sid]).fetchone()[0]==frozen
    too_long=client.post('/api/profiles/text-revisions',json={'kind':'prompt','name':'Лимит','text':'Ё'*16001})
    assert too_long.status_code==409 and too_long.json()['detail']['code']=='profile_text_too_long'
    assert client.post('/api/profiles/revisions',json={**body,'tools':'web_search'}).status_code==422


def test_native_registry_preserves_only_explicit_enabled_mcp_tools_and_denies_remote_schema_refs(profiles):
    client,database,_=profiles
    tool={'name':'echo','title':'Вернуть текст','description':'Read a selected record',
        'inputSchema':{'type':'object','properties':{'text':{'type':'string'}},'required':['text']},
        'annotations':{'readOnlyHint':True}}
    with sqlite3.connect(database) as db:
        db.execute('CREATE TABLE les_mcp_connections(id TEXT PRIMARY KEY,name TEXT,url TEXT,tools TEXT,config TEXT)')
        db.execute('INSERT INTO les_mcp_connections VALUES(?,?,?,?,?)',['fixture','Мой MCP','http://127.0.0.1:1/mcp',json.dumps([tool]),'{}'])
    name='mcp_'+hashlib.sha256(b'fixture:echo').hexdigest()[:32]
    registry=client.get('/api/tools/registry',params={'category':'mcp'}).raise_for_status().json()
    assert len(registry['tools'])==1 and registry['tools'][0]['name']==name
    assert registry['tools'][0]['input_schema']==tool['inputSchema']
    catalog=client.get('/api/profiles').raise_for_status().json()
    active=next(v for v in catalog['profiles'] if v['mode']=='agent')['active']
    assert name not in active['tools']
    published=client.post('/api/profiles/revisions',json={'mode':'agent','name':'MCP выбран',
        'prompt_revision_id':active['prompt_revision_id'],'skill_revision_id':active['skill_revision_id'],'tools':[name]})
    assert published.raise_for_status().json()['tools']==[name]
    tool['inputSchema']={'type':'object','properties':{'text':{'$ref':'https://127.0.0.1:1/schema'}}}
    with sqlite3.connect(database) as db:
        db.execute('UPDATE les_mcp_connections SET tools=?',[json.dumps([tool])])
    assert client.get('/api/tools/registry').status_code==503
