"""Compatibility codec: original-file authority and containment are native."""
from pathlib import Path

from backend import native_core
from backend.runtime_paths import mutable_path


def resolve_original(document: dict, *, content_dir: Path | None = None) -> Path | None:
    storage = Path(content_dir) if content_dir is not None else mutable_path("storage/datasets")
    response = native_core.required('document_source', request={
        'document': {key: str(document.get(key) or '') for key in ('source_path', 'dataset_id', 'file_name')},
        'content_root': str(storage.resolve()),
    })
    return Path(response['path']) if response.get('status') == 'ok' else None
