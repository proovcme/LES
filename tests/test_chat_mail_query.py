"""Mail answer ownership is checked through the shipping Rust chat and reader."""
from email.message import EmailMessage
import hashlib
import json

import pytest
from tests.test_light_native_chat_turn import native_turn
from tests.test_light_native_api import wait_for


def _write_message(path, *, subject, sender, to, message_id, body, date):
    message = EmailMessage()
    for key, value in [('Subject', subject), ('From', sender), ('To', to),
                       ('Message-ID', message_id), ('Date', date)]:
        message[key] = value
    message.set_content(body)
    path.write_bytes(message.as_bytes())


@pytest.mark.parametrize('native_turn', ['rag-small'], indirect=True)
def test_chat_does_not_replace_model_with_deterministic_mail_answer(native_turn, tmp_path):
    t = native_turn
    folder = tmp_path / 'Письма с пробелами'
    folder.mkdir()
    original = folder / '01.eml'
    _write_message(original, subject='Dropbox notice',
                   sender='Dropbox <no-reply@example.com>', to='User <user@example.com>',
                   message_id='<m1@example.com>', body='Welcome to Dropbox.',
                   date='Tue, 26 May 2026 09:00:00 +0300')
    before = original.read_bytes()
    digest = hashlib.sha256(before).hexdigest()
    imported = t.client.post('/api/mail/import-local', json={
        'source_folder': str(folder), 'parse': True,
    }).raise_for_status().json()
    assert imported['files'] == 1
    dataset = imported['dataset_id']
    import sqlite3
    def indexed_document():
        with sqlite3.connect(t.database) as db:
            return db.execute("SELECT id FROM documents WHERE dataset_id=? AND status='INDEXED' AND chunk_count>0", [dataset]).fetchone()
    try:
        wait_for(indexed_document, timeout=40)
    except AssertionError:
        raise AssertionError(t.client.get('/api/mail/accounts').json())
    doc_id = indexed_document()[0]
    t.scripts.extend([
        {'tool': 'read_source', 'args': {'doc_id': doc_id, 'q': 'Dropbox', 'max_chars': 4000}},
        {'text': 'MODEL_OWNED_MAIL_ANSWER [Источник 1]'},
    ])
    response = t.client.post('/api/chat', json={
        'question': 'Найди письма про Dropbox', 'session_id': 'native-mail-owner',
        'mode': 'agent', 'retrieval_mode': 'pull',
        'scope': {'scope_type': 'dataset', 'dataset_ids': [dataset], 'selected_sources_only': True},
    }).raise_for_status()
    assert response.headers['x-les-handler'] == 'rust.chat'
    result = response.json()
    assert result['answer'] == 'MODEL_OWNED_MAIL_ANSWER [Источник 1]'
    assert result['model']['model_id'] == 'fixture:9b'
    assert result['model']['revision_id'] == t.revision.revision_id
    assert result['model']['fallback_used'] is False
    assert 'norm_base' not in result['model'] and result['crag_status'] != 'DETERMINISTIC'
    assert len(t.seen) == 2 and not t.scripts
    assert all(request['model'] == 'fixture:9b' for request in t.seen)
    assert 'Welcome to Dropbox.' in json.dumps(t.seen[-1]['messages'], ensure_ascii=False)
    assert result['artifact']['tool_results'][0]['status'] == 'ok'
    assert result['source_map'][0]['doc_id'] == doc_id
    assert result['source_map'][0]['source_sha256'] == digest
    assert t.client.get(result['source_map'][0]['url']).raise_for_status().content == before
    assert original.read_bytes() == before
    history = t.client.get('/api/chat/history', params={'session_id': 'native-mail-owner'}).raise_for_status().json()
    assert 'MODEL_OWNED_MAIL_ANSWER' in json.dumps(history, ensure_ascii=False)
