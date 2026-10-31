"""Register source identity and filesystem metadata; parsing belongs to ingestion."""
from pathlib import Path


def register(catalog, content_dir, dataset_id, source_path, file_name, *, force_reindex=False):
    relative = Path(file_name)
    if relative.is_absolute() or '..' in relative.parts:
        raise ValueError(f'unsafe file_name: {file_name}')
    source = Path(source_path).resolve(strict=True)
    stat = source.stat()
    if not source.is_file():
        raise ValueError('External source must be a file')
    (content_dir / dataset_id).mkdir(parents=True, exist_ok=True)
    document_id, _, _ = catalog.add_document(
        dataset_id, relative.as_posix(), file_mtime=stat.st_mtime,
        file_size=stat.st_size, source_path=str(source), force_reindex=force_reindex)
    # Do not classify here or reset counters after add_document: a parser may
    # already have claimed this registration. It classifies in its own worker.
    return document_id
