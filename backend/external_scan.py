"""Read-only complete directory enumeration; inaccessible trees never look empty."""
import os
from pathlib import Path
from backend.smart_index import SKIP_DIRS


def source_files(root: Path, *, limit=50000):
    def fail(error): raise error
    count = 0
    for directory, dirs, names in os.walk(root, onerror=fail, followlinks=False):
        dirs[:] = [name for name in dirs if name not in SKIP_DIRS and name != '_originals'
                   and not (Path(directory) / name).is_symlink()]
        for name in sorted(names):
            path = Path(directory) / name
            if path.is_symlink(): continue
            count += 1
            if count > limit:
                raise ValueError('Папка содержит больше 50 000 файлов. Выберите папку меньшего размера.')
            yield path
