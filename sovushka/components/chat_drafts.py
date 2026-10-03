"""Bounded per-conversation drafts in the application's own user storage."""
from __future__ import annotations


class ChatDrafts:
    KEY = "chat_drafts_v1"
    LIMIT = 16
    MAX_CHARS = 20000

    def __init__(self, storage, session_id):
        self.storage = storage
        self.session_id = str(session_id)

    def read(self):
        records = self.storage.get(self.KEY) or {}
        return str(records.get(self.session_id) or "")[:self.MAX_CHARS] if isinstance(records, dict) else ""

    def save(self, text):
        previous = self.storage.get(self.KEY) or {}
        records = dict(previous) if isinstance(previous, dict) else {}
        records.pop(self.session_id, None)
        if text:
            records[self.session_id] = str(text)[:self.MAX_CHARS]
        self.storage[self.KEY] = dict(list(records.items())[-self.LIMIT:])

    def switch(self, session_id, current_text):
        self.save(current_text)
        self.session_id = str(session_id)
        return self.read()
