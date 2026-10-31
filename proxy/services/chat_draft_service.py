"""Port-independent chat drafts with compare-and-swap protection across windows."""
from __future__ import annotations

import json
from datetime import datetime, timezone

from proxy.services import chat_session_service as sessions


def _get(conn, session_id):
    if sessions._get(conn, session_id) is None:
        raise LookupError('Диалог не найден. Откройте список диалогов.')
    if sessions._has_table(conn, 'les_chat_drafts'):
        row = conn.execute('SELECT revision, content_json FROM les_chat_drafts WHERE session_id=?', (session_id,)).fetchone()
        if row:
            return dict(revision=row['revision'], **json.loads(row['content_json']))
    return dict(revision=0, text='', attachment=None)


def get_draft(session_id):
    with sessions._connection() as conn:
        if conn is None:
            raise LookupError('Диалог не найден. Откройте список диалогов.')
        return _get(conn, session_id)


def save_draft(session_id, revision, text, attachment=None):
    with sessions._connection(write=True) as conn:
        current = _get(conn, session_id)
        if current['revision'] != revision:
            raise sessions.SessionConflictError('Черновик изменён в другом окне. Ваш текст сохранён в этом окне; откройте диалог заново, чтобы сравнить версии.')
        conn.execute('''CREATE TABLE IF NOT EXISTS les_chat_drafts (
            session_id TEXT PRIMARY KEY, revision INTEGER NOT NULL,
            content_json TEXT NOT NULL, updated_at TEXT NOT NULL)''')
        content = dict(text=text, attachment=attachment)
        conn.execute('''INSERT INTO les_chat_drafts VALUES (?, ?, ?, ?)
            ON CONFLICT(session_id) DO UPDATE SET revision=excluded.revision,
            content_json=excluded.content_json, updated_at=excluded.updated_at''',
            (session_id, revision + 1, json.dumps(content, ensure_ascii=False), datetime.now(timezone.utc).isoformat()))
        return dict(revision=revision + 1, **content)
