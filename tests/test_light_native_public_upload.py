"""Public file/folder intake, durable receipts and the actual model boundary."""
import json
from email.message import EmailMessage
import sqlite3
from threading import Event, Thread
import httpx
import pytest

from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_api import wait_for

pytestmark=pytest.mark.parametrize('native_turn',['public'],indirect=True)
URL='/api/chat/attachments'

def test_public_upload_survives_restart_and_owns_its_replay_and_read(native_turn):
    t=native_turn
    t.state.joinpath('data').mkdir(exist_ok=True)
    with sqlite3.connect(t.state/'data/les_meta.db') as db:
        db.execute('CREATE TABLE auth_keys(key_value TEXT PRIMARY KEY,role TEXT,expires_at TEXT,is_active INTEGER)')
        db.executemany("INSERT INTO auth_keys VALUES (?,'user',NULL,1)",[('upload-a',),('upload-b',)])
    a={'X-Api-Key':'upload-a','Idempotency-Key':'fixture-file-key'}
    b={'X-Api-Key':'upload-b','Idempotency-Key':'fixture-file-key'}
    content='Документ 日本語'.encode()
    files={'file':('Файл с пробелом.txt',content)}
    response=t.client.post(URL,files=files,headers=a)
    assert response.status_code==201 and response.headers['x-les-handler']=='rust.attachments'
    saved=response.json()
    assert saved['text']==content.decode()
    temp=t.state/'storage/upload-tmp'
    assert not list(temp.iterdir())
    release=Event()
    def partial():
        yield b'--stopped\r\nContent-Disposition: form-data; name="file"; filename="stopped.txt"\r\n\r\n'+b'x'*65536
        release.wait(timeout=15)
    def interrupted():
        try:
            t.client.post(URL,headers={**a,'Content-Type':'multipart/form-data; boundary=stopped'},content=partial())
        except httpx.HTTPError:
            pass
    thread=Thread(target=interrupted,daemon=True);thread.start()
    try:
        wait_for(lambda:len(list(temp.glob('native-read_*.txt')))==1)
        leftover=next(temp.glob('native-read_*.txt'))
        t.children[-1].kill();t.children[-1].wait(timeout=10)
        assert leftover.exists()
    finally:
        release.set();thread.join(timeout=10)
    (temp/'unrelated.txt').write_bytes(b'leave alone')
    t.start()
    assert t.client.post(URL,files=files,headers=a).raise_for_status().json()==saved
    assert not leftover.exists() and (temp/'unrelated.txt').exists()
    assert t.client.post(URL,files={'file':('Файл с пробелом.txt',b'different')},headers=a).status_code==409
    request={'question':'Прочти файл','attachment_id':saved['attachment_id'],'attachment_context':'FORGED PREVIEW'}
    denied=t.client.post('/api/chat',json=request,headers={'X-Api-Key':'upload-b'})
    assert denied.json()['code']=='ATTACHMENT_NOT_FOUND' and not t.seen
    success=t.client.post('/api/chat',json=request,headers={'X-Api-Key':'upload-a'}).raise_for_status().json()
    assert success['artifact']['attachment']['attachment_id']==saved['attachment_id']
    sent=json.dumps(t.seen,ensure_ascii=False)
    assert 'Документ 日本語' in sent and 'FORGED PREVIEW' not in sent
    assert t.client.post(URL,files=files,headers=b).raise_for_status().json()['attachment_id']!=saved['attachment_id']
    cards=list((t.state/'storage/chat_attachments').glob('*.json'))
    assert len(cards)==2 and all('fixture-file-key' not in card.read_text(encoding='utf-8') for card in cards)
    card_path=t.state/'storage/chat_attachments'/f"{saved['attachment_id']}.json"
    stored=json.loads(card_path.read_text(encoding='utf-8'))['stored_name']
    with (t.state/'.env').open('a',encoding='utf-8') as file:file.write('LES_CHAT_ATTACHMENT_MAX_AGE_SEC=0\n')
    assert t.client.post(URL,files=files,headers=a).json()['code']=='ATTACHMENT_EXPIRED'
    tombstone=json.loads(card_path.read_text(encoding='utf-8'))
    assert tombstone['upload_expired'] is True and 'upload_response' not in tombstone
    assert not (card_path.parent/stored).exists()
    assert t.client.post(URL,files={'file':('Файл с пробелом.txt',b'different')},headers=a).status_code==409


def test_quick_alias_and_folder_deliver_complete_snapshot_without_index(native_turn):
    t=native_turn
    assert t.client.get('/api/documents/datasets').raise_for_status().json()=={'datasets':[]}
    folder=t.state/'Папка 日本語';folder.mkdir()
    one=folder/'01 начало.txt';two=folder/'02 конец.txt'
    one.write_text('Первый документ\n'+'a'*11000,encoding='utf-8')
    two.write_text('Второй документ\n'+'b'*11000+'\nFINAL_MARKER',encoding='utf-8')
    originals={p.name:p.read_bytes() for p in folder.iterdir()}
    with (t.state/'.env').open('a',encoding='utf-8') as file:
        file.write(f'LES_EXTERNAL_ALLOW_ANY=false\nLES_EXTERNAL_SOURCE_ROOTS="{folder}"\n')
    quick=t.client.post('/api/rag/attach?mode=quick',files={'file':('a.txt',b'read only')}).raise_for_status().json()
    assert quick['mode']=='read' and quick['text']=='read only'
    headers={'Idempotency-Key':'folder-test-key'}
    result=t.client.post('/api/rag/attach-folder',json={'path':str(folder)},headers=headers).raise_for_status().json()
    assert result['kind']=='folder' and result['file_count']==2 and result['files']==sorted(originals)
    assert result['chars']>20000 and not result['truncated']
    assert t.client.post('/api/rag/attach-folder',json={'path':str(folder)},headers=headers).raise_for_status().json()==result
    assert t.client.post('/api/rag/attach-folder',json={'path':str(t.state)}).status_code==403
    chat=t.client.post('/api/chat',json={'question':'Прочти весь комплект','attachment_id':result['attachment_id']})
    assert chat.status_code==200, chat.json()
    assert 'FINAL_MARKER' in json.dumps(t.seen)
    assert {p.name:p.read_bytes() for p in folder.iterdir()}==originals
    with sqlite3.connect(t.database) as db:
        assert db.execute("SELECT count(*) FROM sqlite_master WHERE name='documents'").fetchone()[0]==0
    two.write_bytes(b'\0binary')
    rejected=t.client.post('/api/rag/attach-folder',json={'path':str(folder)})
    assert rejected.status_code==422 and rejected.json()['code']=='ATTACHMENT_FOLDER_INCOMPLETE'
    assert len(list((t.state/'storage/chat_attachments').glob('*.json')))==2
    assert not list((t.state/'storage/upload-tmp').iterdir())


def test_native_saved_mail_reads_mime_without_touching_any_mailbox(native_turn):
    mail=EmailMessage()
    mail['Subject']='Проверка письма'
    mail['From']='Sender <sender@example.invalid>'
    mail['To']='Recipient <recipient@example.invalid>'
    mail.set_content('Тело письма: проект 日本語')
    mail.add_attachment('Строка из вложения',subtype='plain',filename='Заметка.txt')
    mail.add_attachment(b'%PDF test',maintype='application',subtype='pdf',filename='Не разобран.pdf')
    original=mail.as_bytes()
    for suffix, content in [('eml',original),('emlx',str(len(original)).encode()+b'\n'+original+b'<plist>ignored</plist>')]:
        result=native_turn.client.post(URL,files={'file':('Письмо.'+suffix,content)}).raise_for_status().json()
        assert all(text in result['text'] for text in ['Проверка письма','проект 日本語','Строка из вложения',
            'Не разобран.pdf','Содержимое вложения не прочитано.'])
        assert 'ignored' not in result['text']
