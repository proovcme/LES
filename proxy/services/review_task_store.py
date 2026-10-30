"""Scoped review inventories and immutable conclusions, independent of chat memory."""
from __future__ import annotations

import json
from uuid import uuid4

from proxy.services.document_task_store import digest
from proxy.services.document_task_database import DocumentTaskDatabase
from proxy.services.tabular_document_service import encode


class ReviewTaskStore(DocumentTaskDatabase):
    def __init__(self, path):
        super().__init__(path)
        with self.connect() as db:
            db.executescript('''
                CREATE TABLE IF NOT EXISTS review_tasks (
                    id TEXT PRIMARY KEY, owner TEXT NOT NULL, identity TEXT UNIQUE NOT NULL,
                    question TEXT NOT NULL, units TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE IF NOT EXISTS review_events (
                    task_id TEXT NOT NULL REFERENCES review_tasks(id), unit_id TEXT NOT NULL,
                    revision INTEGER NOT NULL, fingerprint TEXT NOT NULL, observation TEXT NOT NULL,
                    PRIMARY KEY(task_id,revision));
                CREATE INDEX IF NOT EXISTS review_latest ON review_events(task_id,unit_id,revision);
                CREATE TABLE IF NOT EXISTS review_reads (
                    task_id TEXT NOT NULL REFERENCES review_tasks(id), unit_id TEXT NOT NULL,
                    fingerprint TEXT NOT NULL, fragment TEXT NOT NULL, start INTEGER NOT NULL, stop INTEGER NOT NULL,
                    PRIMARY KEY(task_id,unit_id,fingerprint,fragment,start,stop));
                CREATE TABLE IF NOT EXISTS review_receipts (
                    task_id TEXT NOT NULL REFERENCES review_tasks(id), request_id TEXT NOT NULL,
                    digest TEXT NOT NULL, revision INTEGER NOT NULL, PRIMARY KEY(task_id,request_id));
            ''')

    def create(self, owner, question, selectors):
        if not isinstance(question, str) or not question.strip() or len(question) > 20000:
            raise ValueError('Нужна цель проверки до 20000 символов')
        if not isinstance(selectors, list) or not 1 <= len(selectors) <= 32:
            raise ValueError('Выберите от 1 до 32 документов или разделов')
        units = [{'unit_id': digest(selector), 'selector': selector} for selector in selectors]
        if len({unit['unit_id'] for unit in units}) != len(units):
            raise ValueError('Список проверки содержит повторные разделы')
        identity = digest([owner, question, sorted(units, key=lambda unit: unit['unit_id'])])
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            existing = db.execute('SELECT id FROM review_tasks WHERE identity=?', (identity,)).fetchone()
            if existing:
                return existing['id']
            task_id = uuid4().hex
            db.execute('INSERT INTO review_tasks(id,owner,identity,question,units) VALUES(?,?,?,?,?)',
                       (task_id, owner, identity, question, encode(units)))
            return task_id

    def task(self, owner, task_id):
        with self.connect() as db:
            row = db.execute('SELECT * FROM review_tasks WHERE owner=? AND id=?', (owner, task_id)).fetchone()
            if row is None:
                raise ValueError('Проверка недоступна в текущем диалоге и области документов')
            return {**dict(row), 'units': json.loads(row['units'])}

    def recent_reviews(self, owner):
        with self.connect() as db:
            return [dict(row) for row in db.execute(
                'SELECT id AS task_id,substr(question,1,300) AS goal,revision FROM review_tasks '
                'WHERE owner=? ORDER BY rowid DESC LIMIT 5', (owner,))]

    def history(self, owner, task_id, offset=0):
        self.task(owner, task_id)
        if type(offset) is not int or offset < 0:
            raise ValueError('Некорректное смещение истории')
        with self.connect() as db:
            rows = db.execute('SELECT * FROM review_events WHERE task_id=? ORDER BY revision LIMIT 9 OFFSET ?',
                              (task_id, offset)).fetchall()
        return {'events': [self._event(row) for row in rows[:8]],
                'next_offset': offset + 8 if len(rows) > 8 else None}

    @staticmethod
    def _event(row):
        return {**dict(row), 'observation': json.loads(row['observation'])}

    def latest_events(self, owner, task_id):
        self.task(owner, task_id)
        with self.connect() as db:
            rows = db.execute('SELECT * FROM review_events WHERE task_id=? AND revision IN '
                              '(SELECT MAX(revision) FROM review_events WHERE task_id=? GROUP BY unit_id)',
                              (task_id, task_id))
            return {row['unit_id']: self._event(row) for row in rows}

    def status(self, owner, task_id, fingerprints):
        task = self.task(owner, task_id)
        events = self.latest_events(owner, task_id)
        computed = {}

        def state(unit_id, visiting=frozenset()):
            if unit_id not in computed:
                computed[unit_id] = evaluate(unit_id, visiting)
            return computed[unit_id]

        def evaluate(unit_id, visiting):
            event = events.get(unit_id)
            if not event:
                return 'pending'
            if unit_id in visiting or not fingerprints.get(unit_id) or event['fingerprint'] != fingerprints[unit_id]:
                return 'needs_recheck'
            observation = event['observation']
            for dependency, revision in observation['dependencies'].items():
                other = events.get(dependency)
                if (not other or other['revision'] != revision or
                        state(dependency, visiting | {unit_id}) != 'reviewed'):
                    return 'needs_recheck'
            return observation['status']

        units = [{**unit, 'state': state(unit['unit_id']),
                  'result_revision': events.get(unit['unit_id'], {}).get('revision')}
                 for unit in task['units']]
        counts = {name: sum(unit['state'] == name for unit in units)
                  for name in ('pending', 'reviewed', 'needs_review', 'excluded', 'needs_recheck')}
        next_unit = next((unit['unit_id'] for name in ('needs_recheck', 'pending')
                          for unit in units if unit['state'] == name), None)
        return {'task_id': task_id, 'revision': task['revision'], 'units': units, 'total': len(units), **counts,
                'next_unit_id': next_unit, 'coverage_complete': not counts['pending'] and not counts['needs_recheck'],
                'needs_attention': bool(counts['needs_review'] or counts['needs_recheck']),
                'accepted': False, 'scope': 'explicit_inventory_only', 'conclusions_are_model_authored': True}

    def remember_read(self, owner, task_id, unit_id, snapshot, fragment, start, stop):
        task = self.task(owner, task_id)
        selectors = {unit['unit_id']: unit['selector'] for unit in task['units']}
        if unit_id not in selectors or selectors[unit_id] != snapshot['selector']:
            raise ValueError('Раздел не входит в список проверки')
        with self.connect() as db:
            db.execute('INSERT OR IGNORE INTO review_reads VALUES(?,?,?,?,?,?)',
                       (task_id, unit_id, snapshot['fingerprint'], fragment, start, stop))

    def record(self, owner, task_id, *, request_id, expected_revision, unit_id, snapshot,
               source_fingerprint, status, note, evidence, dependencies):
        if (not isinstance(request_id, str) or not 1 <= len(request_id) <= 120 or
                type(expected_revision) is not int or expected_revision < 0):
            raise ValueError('Нужны request_id и текущая ревизия проверки')
        if (status not in {'reviewed', 'needs_review', 'excluded'} or not isinstance(note, str) or
                not note.strip() or len(note) > 1200):
            raise ValueError('Нужны статус и объяснение до 1200 символов')
        if (not isinstance(dependencies, list) or len(dependencies) > 16 or
                any(not isinstance(value, str) for value in dependencies) or len(set(dependencies)) != len(dependencies)):
            raise ValueError('Некорректные зависимости вывода')
        if not isinstance(evidence, list) or not 1 <= len(evidence) <= 4:
            raise ValueError('Нужны от 1 до 4 точных цитат прочитанного раздела')
        body = [expected_revision, unit_id, source_fingerprint, status, note, evidence, dependencies]
        with self.connect() as db:
            db.execute('BEGIN IMMEDIATE')
            task = self.task(owner, task_id)
            receipt = db.execute('SELECT * FROM review_receipts WHERE task_id=? AND request_id=?',
                                 (task_id, request_id)).fetchone()
            if receipt:
                if receipt['digest'] != digest(body):
                    raise ValueError('request_id уже использован для другого результата')
                return receipt['revision']
            if task['revision'] != expected_revision:
                raise ValueError('Ревизия проверки изменилась; прочитайте status')
            ids = {unit['unit_id'] for unit in task['units']}
            if unit_id not in ids or unit_id in dependencies or any(dep not in ids for dep in dependencies):
                raise ValueError('Неизвестный раздел или зависимость от самого себя')
            if snapshot['selector'] != next(unit['selector'] for unit in task['units'] if unit['unit_id'] == unit_id):
                raise ValueError('Источник не принадлежит выбранному разделу')
            if source_fingerprint != snapshot['fingerprint']:
                raise ValueError('Источник изменился после чтения; дочитайте текущую версию')
            spans = {}
            for row in db.execute('SELECT fragment,start,stop FROM review_reads '
                                  'WHERE task_id=? AND unit_id=? AND fingerprint=? ORDER BY start',
                                  (task_id, unit_id, source_fingerprint)):
                spans.setdefault(row['fragment'], []).append((row['start'], row['stop']))
            fragments = {item['id']: item['text'] for item in snapshot['fragments']}
            for item in evidence:
                if (not isinstance(item, dict) or set(item) != {'fragment_id', 'quote'} or
                        not isinstance(item['quote'], str) or not 1 <= len(item['quote']) <= 300):
                    raise ValueError('Укажите fragment_id и точную цитату до 300 символов')
                text = fragments.get(item['fragment_id'], '')
                if not any(item['quote'] in text[start:stop] for start, stop in spans.get(item['fragment_id'], [])):
                    raise ValueError('Цитата отсутствует в прочитанной части раздела')
            if status == 'reviewed':
                for fragment, text in fragments.items():
                    end = 0
                    for start, stop in spans.get(fragment, []):
                        if start > end:
                            break
                        end = max(end, stop)
                    if end < len(text):
                        raise ValueError('Раздел прочитан не полностью; продолжите read либо выберите needs_review')
            events = self.latest_events(owner, task_id)
            if any(dep not in events or events[dep]['observation']['status'] != 'reviewed' for dep in dependencies):
                raise ValueError('Зависимый вывод ещё не проверен')
            observation = {'status': status, 'note': note, 'evidence': evidence,
                           'dependencies': {dep: events[dep]['revision'] for dep in dependencies},
                           'source': {'selector': snapshot['selector'], 'sha256': snapshot['source_sha256']},
                           'evidence_provenance': [{'fragment_id': item['fragment_id'],
                               'locator': next(fragment['locator'] for fragment in snapshot['fragments']
                                               if fragment['id'] == item['fragment_id'])} for item in evidence]}
            revision = expected_revision + 1
            db.execute('INSERT INTO review_events VALUES(?,?,?,?,?)',
                       (task_id, unit_id, revision, source_fingerprint, encode(observation)))
            db.execute('UPDATE review_tasks SET revision=? WHERE id=?', (revision, task_id))
            db.execute('INSERT INTO review_receipts VALUES(?,?,?,?)', (task_id, request_id, digest(body), revision))
            return revision
