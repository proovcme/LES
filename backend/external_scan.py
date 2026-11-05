"""Read-only complete directory enumeration; inaccessible trees never look empty."""
import os
from pathlib import Path
from backend.smart_index import SKIP_DIRS


def source_files(root: Path, *, limit=50000):
    from backend import native_core
    if native_core.supports('scan_sources'):
        resolved = root.resolve(strict=True)
        try:
            result = native_core.call('scan_sources', path=str(resolved),
                limit=limit, skip_directories=sorted(SKIP_DIRS | {'_originals'}))
        except native_core.NativeCoreError as error:
            if str(error) in {'CORE_SCAN_FILE_LIMIT','CORE_SCAN_OUTPUT_LIMIT'}:
                raise ValueError('Папка слишком велика для полного обхода. Выберите папку меньшего размера.') from error
            raise OSError('Не удалось полностью прочитать исходную папку.') from error
        if result.get('complete') is not True or not isinstance(result.get('files'), list):
            raise OSError('Неполный результат обхода исходной папки.')
        for name in result['files']:
            path = Path(name)
            if not path.is_absolute() or not path.is_relative_to(resolved):
                raise OSError('Обход вышел за границу исходной папки.')
            yield path
        return
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
