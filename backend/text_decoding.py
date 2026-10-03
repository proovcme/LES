"""Strict document decoding: preserve Unicode and reject uncertain legacy text."""
from __future__ import annotations

import codecs
from pathlib import Path


def decode_document_text(data: bytes) -> str:
    if not data:
        return ''
    encoding = next((name for marker, name in (
        (codecs.BOM_UTF32_LE, 'utf-32'), (codecs.BOM_UTF32_BE, 'utf-32'),
        (codecs.BOM_UTF16_LE, 'utf-16'), (codecs.BOM_UTF16_BE, 'utf-16'),
        (codecs.BOM_UTF8, 'utf-8-sig'),
    ) if data.startswith(marker)), 'utf-8')
    try:
        text = data.decode(encoding, errors='strict')
    except UnicodeDecodeError:
        if encoding != 'utf-8':
            raise ValueError('Повреждённый текст в заявленной кодировке. Сохраните копию в UTF-8.')
        from charset_normalizer import from_bytes
        candidates = from_bytes(data)
        detected = candidates.best()
        if detected is None or detected.coherence < 0.1 or detected.chaos > 0.1:
            raise ValueError('Не удалось уверенно определить кодировку. Сохраните копию в UTF-8.')
        if any(candidate.coherence >= detected.coherence - 0.03
               and candidate.chaos <= detected.chaos + 0.01 and str(candidate) != str(detected)
               for candidate in candidates):
            raise ValueError('Кодировка неоднозначна. Сохраните копию в UTF-8, чтобы не исказить текст.')
        text = data.decode(detected.encoding, errors='strict')
    if any(ord(char) < 32 and char not in '\t\r\n\f' for char in text):
        raise ValueError('Файл содержит двоичные данные вместо текста.')
    return text


def read_document_text(path: Path) -> str:
    return decode_document_text(path.read_bytes())
