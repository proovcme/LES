"""Real multipart intake, verified originals and native table work without application Python."""
import hashlib
import json
import sqlite3

import pytest

from tests.test_light_native_chat_turn import native_turn, PATH, HEADERS

UPLOAD = '/api/light/internal-attachments/upload'


def upload(t, name, data):
    return t.client.post(UPLOAD, files={'file': (name, data)}, headers=HEADERS).raise_for_status().json()


def test_json_upload_keeps_original_and_chat_rebinds_preview_before_model_failure(native_turn):
    t = native_turn
    raw = '{"проверка":"原本 😀","значение":0}'.encode()
    assert t.client.post(UPLOAD, files={'file': ('source.json', raw)}).status_code == 404
    asset = upload(t, 'Документ 日本語.json', raw)
    root = t.state/'storage/chat_attachments'
    card = json.loads((root/(asset['attachment_id']+'.json')).read_text(encoding='utf-8'))
    assert card['sha256'] == hashlib.sha256(raw).hexdigest()
    assert (root/card['stored_name']).read_bytes() == raw
    assert card['stored_name'] != asset['attachment_id']+'.json'
    t.scripts.append({'text': 'часть ответа', 'broken': True})
    response = t.client.post(PATH, json={'question': 'Прочти вложение', 'attachment_id': asset['attachment_id'],
        'attachment_context': 'FORGED PREVIEW', 'session_id': 'attachment-failure'}, headers=HEADERS)
    assert response.status_code == 503 and response.json()['code'] == 'UPSTREAM_STREAM_INTERRUPTED'
    sent = json.dumps(t.seen, ensure_ascii=False)
    assert '原本 😀' in sent and 'FORGED PREVIEW' not in sent
    with sqlite3.connect(t.database) as db:
        row = db.execute('SELECT attachment_context,artifact_json FROM chat_history').fetchone()
    assert '原本 😀' in row[0] and 'FORGED PREVIEW' not in row[0]
    assert 'UPSTREAM_STREAM_INTERRUPTED' in row[1]
    assert not list((t.state/'storage/upload-tmp').iterdir())


def test_image_is_decoded_and_sent_only_to_model_with_hash_bound_artifact(native_turn):
    from io import BytesIO
    from PIL import Image
    t = native_turn
    image = BytesIO(); Image.new('RGB', (3, 2), '#386847').save(image, format='PNG')
    raw = image.getvalue(); asset = upload(t, 'Картинка.png', raw)
    result = t.client.post(PATH, json={'question': 'Что видно?', 'attachment_id': asset['attachment_id']}, headers=HEADERS).raise_for_status().json()
    content = [m['content'] for m in t.seen[0]['messages'] if m['role'] == 'user'][-1]
    assert isinstance(content, list) and content[-1]['image_url']['url'].startswith('data:image/png;base64,')
    assert result['artifact']['attachment']['source_sha256'] == hashlib.sha256(raw).hexdigest()
    assert 'data:image' not in json.dumps(result)
    broken = t.client.post(UPLOAD, files={'file': ('broken.png', raw[:24])}, headers=HEADERS)
    assert broken.status_code == 422
    assert not list((t.state/'storage/upload-tmp').iterdir())


def test_table_packets_are_saved_in_rust_and_prior_packets_leave_model_context(native_turn):
    t = native_turn
    sid = t.client.post('/api/workspace/sessions', json={'title': 'Таблица'}).raise_for_status().json()['session_id']
    raw = '\n'.join(f'ROW_{i:03};{i}' for i in range(14)).encode()
    asset = upload(t, 'Таблица с пробелами.csv', raw)
    for first in range(1, 15, 6):
        t.scripts.extend([{'tool': 'table_document', 'args': {'operation': 'next'}},
            {'tool': 'table_document', 'args': {'operation': 'record', 'decisions': [
                {'row_id': f's1:r{i}', 'status': 'reviewed', 'note': 'Прочитаны точные ячейки', 'cells': [f'A{i}', f'B{i}']}
                for i in range(first, min(15, first+6))]}}])
    t.scripts.append({'text': 'Готово.'})
    result = t.client.post(PATH, json={'question': 'Проверь каждую строку', 'session_id': sid, 'attachment_id': asset['attachment_id']}, headers=HEADERS).raise_for_status().json()
    receipt = result['artifact']['execution_receipt']
    assert receipt['counts']['reviewed'] == 14 and receipt['status'] == 'recorded' and receipt['accepted'] is False
    assert len(t.seen) == 7
    final = json.dumps(t.seen[-1], ensure_ascii=False)
    assert 'ROW_000' not in final and 'ROW_006' not in final
    with sqlite3.connect(t.state/'data/document-tasks.sqlite3') as db:
        assert db.execute('SELECT COUNT(*) FROM document_rows WHERE decision IS NOT NULL').fetchone()[0] == 14
    # A repeated HTTP receipt replays neither the model nor the table effects.
    assert not list((t.state/'storage/upload-tmp').iterdir())


def test_stopped_table_work_is_incomplete_and_can_resume_after_process_restart(native_turn):
    t = native_turn
    sid = t.client.post('/api/workspace/sessions', json={'title': 'Продолжение'}).raise_for_status().json()['session_id']
    asset = upload(t, 'small.csv', b'one;0\ntwo;1')
    t.scripts.extend([{'tool': 'table_document', 'args': {'operation': 'next'}}, {'text': 'Все проверено.'}])
    first = t.client.post(PATH, json={'question': 'Проверь таблицу', 'session_id': sid, 'attachment_id': asset['attachment_id']}, headers=HEADERS).raise_for_status().json()
    assert first['artifact']['completion_status'] == 'incomplete'
    assert first['artifact']['execution_receipt']['counts']['pending'] == 2
    assert 'не завершена' in first['answer']
    t.children[-1].kill(); t.children[-1].wait(timeout=5); t.start()
    t.scripts.extend([{'tool': 'table_document', 'args': {'operation': 'next'}},
        {'tool': 'table_document', 'args': {'operation': 'record', 'decisions': [
            {'row_id': f's1:r{i}', 'status': 'reviewed', 'note': 'Прочитано', 'cells': [f'A{i}']} for i in (1,2)]}},
        {'text': 'Решения записаны.'}])
    result = t.client.post(PATH, json={'question': 'Продолжи прежнее задание', 'session_id': sid}, headers=HEADERS).raise_for_status().json()
    assert result['artifact']['execution_receipt']['task_id'] == first['artifact']['execution_receipt']['task_id']
    assert result['artifact']['execution_receipt']['counts']['reviewed'] == 2


def test_multipart_rejects_unknown_fields_and_cleans_accepted_file_on_late_error(native_turn):
    t = native_turn
    response = t.client.post(UPLOAD, files=[('file', ('ok.txt', b'text')), ('unexpected', (None, 'bad'))], headers=HEADERS)
    assert response.status_code == 422
    assert not list((t.state/'storage/upload-tmp').iterdir())
    for name in ('../escape.txt', 'C:escape.txt', 'CON.txt'):
        assert t.client.post(UPLOAD, files={'file': (name, b'text')}, headers=HEADERS).status_code == 422
    # A truncated multipart body must not publish a half-uploaded original.
    headers={**HEADERS,'Content-Type':'multipart/form-data; boundary=truncated'}
    body=b'--truncated\r\nContent-Disposition: form-data; name="file"; filename="partial.txt"\r\n\r\nunfinished'
    assert t.client.post(UPLOAD,content=body,headers=headers).status_code==422
    assert not list((t.state/'storage/upload-tmp').iterdir())


def test_upload_limit_stops_stream_and_modified_original_never_reaches_model(native_turn):
    t=native_turn
    large=upload(t,'larger-than-default-body-limit.txt',b'x'*(2*1024*1024+17))
    assert large['truncated'] is True and large['chars']==2*1024*1024+17
    with (t.state/'.env').open('a',encoding='utf-8') as env:env.write('MAX_UPLOAD_MB=1\n')
    rejected=t.client.post(UPLOAD,files={'file':('too-large.txt',b'x'*(1024*1024+1))},headers=HEADERS)
    assert rejected.status_code==422 and rejected.json()['code']=='ATTACHMENT_UPLOAD_LIMIT'
    assert not list((t.state/'storage/upload-tmp').iterdir())
    asset=upload(t,'source.txt',b'original')
    root=t.state/'storage/chat_attachments'
    card=json.loads((root/(asset['attachment_id']+'.json')).read_text(encoding='utf-8'))
    (root/card['stored_name']).write_bytes(b'modified')
    failed=t.client.post(PATH,json={'question':'Прочти','attachment_id':asset['attachment_id'],'attachment_context':'FORGED'},headers=HEADERS)
    assert failed.status_code>=400 and not t.seen
    assert 'Вложение изменилось' in failed.json()['detail']
    with sqlite3.connect(t.database) as db:context=db.execute('SELECT attachment_context FROM chat_history').fetchone()[0]
    assert context==''
