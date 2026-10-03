"""Bounded read-only folder snapshots for a single chat request."""
from pathlib import Path
import os
import tempfile
from uuid import uuid4

from backend.converter import SUPPORTED, convert_to_markdown
from backend.runtime_paths import mutable_path
from backend.smart_index import SKIP_DIRS, is_temporary_source_name
from proxy.services.chat_attachment_service import preserve_read_attachment


def read_folder(root: Path) -> dict:
    files = []
    entries = 0
    def scan_error(error): raise error
    for current, directories, names in os.walk(root, followlinks=False, onerror=scan_error):
        directories[:] = [name for name in directories if name not in SKIP_DIRS
                           and not (Path(current) / name).is_symlink()]
        for name in sorted(names):
            entries += 1
            if entries > 5000:
                raise ValueError('В папке слишком много файлов для одного сообщения. Добавьте её в базу знаний.')
            path = Path(current) / name
            if path.is_symlink() or not path.resolve().is_relative_to(root.resolve()): continue
            if path.suffix.lower() not in SUPPORTED or is_temporary_source_name(name): continue
            files.append(path)
            if len(files) > 25:
                raise ValueError('Для одного сообщения можно прочитать до 25 документов. Добавьте папку в базу знаний.')
    if not files:
        raise ValueError('В папке нет поддерживаемых документов.')
    if sum(path.stat().st_size for path in files) > 50 * 1024 * 1024:
        raise ValueError('Папка велика для одного сообщения. Добавьте её в базу знаний.')
    parts = []
    for path in files:
        text = convert_to_markdown(path)
        if not text:
            raise ValueError(f'Не удалось прочитать «{path.name}». Остальные файлы не будут отправлены как полный комплект.')
        parts.append(f'## {path.relative_to(root).as_posix()}\n\n{text}')
        if sum(map(len, parts)) > 48000:
            raise ValueError('Текст папки не помещается в одно вложение. Добавьте её в базу знаний для поиска по всем документам.')
    text = '\n\n'.join(parts)
    temporary = mutable_path('storage/chat-folder-tmp')
    temporary.mkdir(parents=True, exist_ok=True)
    ident = 'read_' + uuid4().hex[:12]
    with tempfile.TemporaryDirectory(dir=temporary) as directory:
        snapshot = Path(directory) / 'folder.md'
        snapshot.write_text(text, encoding='utf-8')
        preserve_read_attachment(snapshot, attachment_id=ident, original_name=f'{root.name}.md')
    return dict(attachment_id=ident, mode='read', kind='folder', name=root.name,
                text=text, chars=len(text), truncated=False, file_count=len(files),
                files=[path.relative_to(root).as_posix() for path in files])
