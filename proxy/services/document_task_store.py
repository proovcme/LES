"""Transactional, scoped row progress independent of chat history or summaries."""
from __future__ import annotations

from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import sqlite3
from uuid import uuid4

from proxy.services.tabular_document_service import encode


def digest(value):
    return hashlib.sha256(encode(value).encode('utf-8')).hexdigest()


class DocumentTaskStore:
    def __init__(self, path: Path):
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.connect() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS document_tasks (
                    id TEXT PRIMARY KEY, owner TEXT NOT NULL, identity TEXT UNIQUE NOT NULL,
                    question TEXT NOT NULL, source TEXT NOT NULL, scope_key TEXT NOT NULL DEFAULT '',
                    revision INTEGER NOT NULL DEFAULT 0,
                    created TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
                CREATE TABLE IF NOT EXISTS document_rows (
                    task_id TEXT NOT NULL, id TEXT NOT NULL, ordinal INTEGER NOT NULL,
                    source TEXT NOT NULL, decision TEXT,
                    PRIMARY KEY(task_id,id), FOREIGN KEY(task_id) REFERENCES document_tasks(id));
                CREATE TABLE IF NOT EXISTS document_packets (
                    task_id TEXT NOT NULL, id TEXT NOT NULL, revision INTEGER NOT NULL,
                    rows TEXT NOT NULL, receipt TEXT,
                    PRIMARY KEY(task_id,id), FOREIGN KEY(task_id) REFERENCES document_tasks(id));
                CREATE TABLE IF NOT EXISTS document_bindings (
                    scope_key TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES document_tasks(id));
            ''')
            if 'scope_key' not in {row['name'] for row in db.execute('PRAGMA table_info(document_tasks)')}:
                db.execute("ALTER TABLE document_tasks ADD COLUMN scope_key TEXT NOT NULL DEFAULT ''")

    @contextmanager
    def connect(self):
        db = sqlite3.connect(self.path, timeout=15)
        db.row_factory = sqlite3.Row
        try:
            db.execute('PRAGMA foreign_keys=ON')
            db.execute('PRAGMA journal_mode=WAL')
            with db:
                yield db
        finally:
            db.close()

    def open(self, owner: str, question: str, document: dict, *, scope_key='') -> str:
        identity = digest([owner, question, document['sha256'], document['schema']])
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            existing = db.execute('SELECT id FROM document_tasks WHERE identity=?', (identity,)).fetchone()
            if existing:
                if scope_key:
                    db.execute('INSERT OR REPLACE INTO document_bindings VALUES(?,?)', (scope_key, existing['id']))
                return existing['id']
            task_id = uuid4().hex
            metadata = {key: value for key, value in document.items() if key != 'rows'}
            db.execute('INSERT INTO document_tasks(id,owner,identity,question,source,scope_key) VALUES(?,?,?,?,?,?)',
                       (task_id, owner, identity, question, encode(metadata), scope_key))
            db.executemany('INSERT INTO document_rows(task_id,id,ordinal,source) VALUES(?,?,?,?)',
                           [(task_id, row['id'], index, encode(row)) for index, row in enumerate(document['rows'])])
            if scope_key:
                db.execute('INSERT OR REPLACE INTO document_bindings VALUES(?,?)', (scope_key, task_id))
            return task_id

    def latest(self, scope_key):
        with self.connect() as db:
            row = db.execute('SELECT t.id,t.owner,t.source FROM document_tasks t JOIN document_bindings b '
                             'ON b.task_id=t.id WHERE b.scope_key=?', (scope_key,)).fetchone()
            return dict(row) if row else None

    def activate(self, owner, task_id):
        with self.connect() as db:
            task = self._task(db, owner, task_id)
            if task['scope_key']:
                db.execute('INSERT OR REPLACE INTO document_bindings VALUES(?,?)', (task['scope_key'], task_id))

    def new_question(self, owner, task_id, question):
        with self.connect() as db:
            task = self._task(db, owner, task_id)
            document = json.loads(task['source'])
            document['rows'] = [json.loads(row['source']) for row in db.execute(
                'SELECT source FROM document_rows WHERE task_id=? ORDER BY ordinal', (task_id,))]
            scope_key = task['scope_key']
        return self.open(owner, question, document, scope_key=scope_key)

    def _task(self, db, owner, task_id):
        row = db.execute('SELECT * FROM document_tasks WHERE id=? AND owner=?', (task_id, owner)).fetchone()
        if row is None:
            raise ValueError('Задание не принадлежит текущему диалогу, файлу и области источников')
        return row

    def status(self, owner, task_id):
        with self.connect() as db:
            task = self._task(db, owner, task_id)
            rows = db.execute("SELECT COALESCE(json_extract(decision,'$.status'),'pending') AS status, "
                              'COUNT(*) AS count FROM document_rows WHERE task_id=? GROUP BY status',
                              (task_id,)).fetchall()
            counts = {'pending': 0, 'reviewed': 0, 'needs_review': 0, 'excluded': 0}
            for row in rows:
                counts[row['status']] = row['count']
            return {'task_id': task_id, 'revision': task['revision'], 'total': sum(counts.values()), **counts,
                    'coverage_complete': bool(rows) and counts['pending'] == 0,
                    'needs_attention': bool(counts['needs_review']),
                    'decisions_are_model_authored': True, 'source_sha256': json.loads(task['source'])['sha256']}

    def matches_question(self, owner, task_id, question):
        with self.connect() as db:
            return self._task(db, owner, task_id)['question'] == question

    def recent(self, owner):
        with self.connect() as db:
            return [{'task_id': row['task_id'], 'question_preview': row['question'][:300],
                     'question_truncated': len(row['question']) > 300, 'revision': row['revision']} for row in db.execute(
                'SELECT id AS task_id,question,revision FROM document_tasks WHERE owner=? ORDER BY rowid DESC LIMIT 5',
                (owner,))]

    def next_packet(self, owner, task_id, *, max_bytes=6000, max_rows=6):
        """Repeat a pending packet after a crash; only commit advances progress."""
        if not 512 <= max_bytes <= 12000 or not 1 <= max_rows <= 12:
            raise ValueError('Некорректный бюджет пакета')
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            task = self._task(db, owner, task_id)
            candidates = db.execute('SELECT source FROM document_rows WHERE task_id=? AND decision IS NULL '
                                    'ORDER BY ordinal LIMIT ?', (task_id, max_rows)).fetchall()
            rows = []
            for candidate in candidates:
                row = json.loads(candidate['source'])
                if len(encode([*rows, row]).encode('utf-8')) > max_bytes:
                    if not rows:
                        return {'blocked_row': row['id'], 'reason': 'Строка превышает бюджет пакета; '
                                'она не обрезана и не отмечена обработанной', 'rows': [], 'packet_id': None}
                    break
                rows.append(row)
            packet_id = digest([task_id, task['revision'], rows])
            db.execute('INSERT OR IGNORE INTO document_packets(task_id,id,revision,rows) VALUES(?,?,?,?)',
                       (task_id, packet_id, task['revision'], encode([row['id'] for row in rows])))
            return {'packet_id': packet_id, 'revision': task['revision'], 'rows': rows,
                    'source_sha256': json.loads(task['source'])['sha256']}

    def commit(self, owner, task_id, packet_id, decisions):
        if not isinstance(decisions, list) or not 1 <= len(decisions) <= 12:
            raise ValueError('Нужны решения по всем строкам выданного пакета')
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            task = self._task(db, owner, task_id)
            packet = db.execute('SELECT * FROM document_packets WHERE task_id=? AND id=?',
                                (task_id, packet_id)).fetchone()
            if packet is None:
                raise ValueError('Пакет не выдавался для этого задания')
            ids = json.loads(packet['rows'])
            if (any(not isinstance(item, dict) for item in decisions)
                    or len(decisions) != len(ids) or {item.get('row_id') for item in decisions} != set(ids)):
                raise ValueError('Нужно ровно одно решение для каждого исходного ID пакета')
            receipt = encode(sorted(decisions, key=lambda item: item['row_id']))
            if packet['receipt'] is not None:
                if receipt != packet['receipt']:
                    raise ValueError('Этот пакет уже сохранён с другим результатом')
                return  # Exact redelivery is idempotent, including after later packets.
            if packet['revision'] != task['revision']:
                raise ValueError('Пакет устарел; запросите следующий пакет заново')
            for item in decisions:
                if set(item) != {'row_id', 'status', 'note', 'cells'}:
                    raise ValueError('Решение содержит неизвестные поля; исходные значения менять нельзя')
                if item['status'] not in {'reviewed', 'needs_review', 'excluded'}:
                    raise ValueError('Неизвестный статус строки')
                if not isinstance(item['note'], str) or not 1 <= len(item['note']) <= 1200:
                    raise ValueError('Нужно объяснение решения длиной до 1200 символов')
                source = json.loads(db.execute('SELECT source FROM document_rows WHERE task_id=? AND id=?',
                                              (task_id, item['row_id'])).fetchone()['source'])
                coordinates = {cell['coordinate'] for cell in source['cells']}
                if (not isinstance(item['cells'], list) or not item['cells']
                        or any(not isinstance(ref, str) or ref not in coordinates for ref in item['cells'])):
                    raise ValueError('Укажите точные ячейки текущей строки, подтверждающие наблюдение')
                unresolved = any(cell['kind'] == 'error' or cell.get('needs_recalculation') for cell in source['cells'])
                if unresolved and item['status'] == 'reviewed':
                    raise ValueError('В строке ошибка или формула без результата; требуется needs_review')
                db.execute('UPDATE document_rows SET decision=? WHERE task_id=? AND id=?',
                           (encode(item), task_id, item['row_id']))
            db.execute('UPDATE document_packets SET receipt=? WHERE task_id=? AND id=?', (receipt, task_id, packet_id))
            db.execute('UPDATE document_tasks SET revision=revision+1 WHERE id=?', (task_id,))

    def results(self, owner, task_id, *, offset=0, max_bytes=9000):
        if isinstance(offset, bool) or not isinstance(offset, int) or offset < 0:
            raise ValueError('Некорректное смещение')
        with self.connect() as db:
            self._task(db, owner, task_id)
            rows = db.execute('SELECT id,source,decision FROM document_rows WHERE task_id=? '
                              'ORDER BY ordinal LIMIT 12 OFFSET ?', (task_id, offset)).fetchall()
            output = []
            for row in rows:
                item = {'source': json.loads(row['source']),
                        'decision': json.loads(row['decision']) if row['decision'] else None}
                if len(encode([*output, item]).encode('utf-8')) > max_bytes:
                    if not output:
                        return {'rows': [], 'next_offset': offset, 'blocked_row': row['id']}
                    break
                output.append(item)
            total = db.execute('SELECT COUNT(*) FROM document_rows WHERE task_id=?', (task_id,)).fetchone()[0]
            return {'rows': output, 'next_offset': offset + len(output) if offset + len(output) < total else None}
