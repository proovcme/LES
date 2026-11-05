"""Small, rotating lifecycle evidence; never provider secrets or document contents."""
import json
import logging
from logging.handlers import RotatingFileHandler
import os
from pathlib import Path
import threading
import time
import uuid


class RuntimeJournal:
    def __init__(self, state):
        folder = Path(state) / 'logs'
        folder.mkdir(parents=True, exist_ok=True)
        self.run_id = uuid.uuid4().hex
        self.lock = threading.Lock()
        self.handler = RotatingFileHandler(folder / 'lifecycle.jsonl', maxBytes=1_048_576,
                                           backupCount=3, encoding='utf-8', delay=True)
        self.handler.setFormatter(logging.Formatter('%(message)s'))

    def record(self, event, **fields):
        # Callers supply stable codes/counters only. Exceptions are classified
        # by type, never serialized with their arbitrary URLs or credentials.
        record = dict(schema='les.lifecycle.v1', at=time.time(), component='launcher',
                      pid=os.getpid(), run_id=self.run_id, event=event, **fields)
        with self.lock:
            message = logging.LogRecord('les.lifecycle', logging.INFO, '', 0,
                json.dumps(record, ensure_ascii=False), (), None)
            self.handler.emit(message)
            self.handler.flush()

    def close(self):
        self.handler.close()
