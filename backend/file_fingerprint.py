"""Read-only byte fingerprints with replacement/change checks around the read."""
import hashlib
import os
from pathlib import Path
import re
import stat
from backend import native_core


def fingerprint(path):
    path = Path(path)
    try:
        result = native_core.call('fingerprint', path=str(path.resolve()))
    except native_core.NativeCoreError as error:
        if str(error) in {'CORE_FILE_UNAVAILABLE', 'CORE_FILE_CHANGED', 'CORE_NOT_REGULAR_FILE'}:
            raise OSError('Оригинал недоступен или изменился во время чтения') from error
        raise
    if result is not None:
        if (result.get('stable') is not True or type(result.get('size')) is not int or result['size'] < 0
                or not isinstance(result.get('sha256'), str) or not re.fullmatch('[0-9a-f]{64}', result['sha256'])):
            raise native_core.NativeCoreError('CORE_FILE_RESPONSE_INVALID')
        return result
    if not stat.S_ISREG(path.stat().st_mode):
        raise OSError('Источник не является обычным файлом')
    with path.open('rb') as stream:
        before = os.fstat(stream.fileno())
        checksum = hashlib.file_digest(stream, 'sha256').hexdigest()
        after = os.fstat(stream.fileno())
        current = path.stat()
    identity = lambda item: (item.st_dev, item.st_ino, item.st_size, item.st_mtime_ns)
    if identity(before) != identity(after) or identity(before) != identity(current):
        raise OSError('Оригинал изменился во время чтения')
    return {'sha256': checksum, 'size': before.st_size, 'stable': True}
