"""IMAP4rev1 modified UTF-7 mailbox names (RFC 3501 section 5.1.3)."""
import base64
import re


def encode_mailbox(text: str) -> str:
    parts, pending = [], []
    def flush():
        if pending:
            raw = ''.join(pending).encode('utf-16-be')
            parts.append('&' + base64.b64encode(raw).decode('ascii').rstrip('=').replace('/', ',') + '-')
            pending.clear()
    for char in text:
        if 32 <= ord(char) <= 126:
            flush()
            parts.append('&-' if char == '&' else char)
        else:
            pending.append(char)
    flush()
    return ''.join(parts)


def decode_mailbox(text: str) -> str:
    def decode(match):
        encoded = match[1]
        if not encoded: return '&'
        raw = encoded.replace(',', '/')
        return base64.b64decode(raw + '=' * (-len(raw) % 4), validate=True).decode('utf-16-be')
    return re.sub(r'&([A-Za-z0-9+,]*)-', decode, text)
