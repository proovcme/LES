"""Stable extraction failures, never searchable substitute document text."""
import json
from pathlib import Path

MESSAGES = json.loads((Path(__file__).resolve().parents[1] / 'config/extraction-errors.json').read_text(encoding='utf-8'))


class DocumentExtractionError(RuntimeError):
    def __init__(self, code, *, line=None, column=None):
        self.code = code
        message = MESSAGES.get(code, 'Не удалось прочитать структуру документа. Проверьте исходный файл.')
        if isinstance(line, int) and line>0 and isinstance(column, int) and column>0:
            message += f' Строка {line}, позиция {column}.'
        super().__init__(message)
