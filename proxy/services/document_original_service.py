"""Resolve originals from trusted catalog records."""
from pathlib import Path

from backend.runtime_paths import mutable_path


def resolve_original(document: dict, *, content_dir: Path | None = None) -> Path | None:
    recorded = str(document.get("source_path") or "").strip()
    if recorded:
        candidate = Path(recorded).expanduser()
        if candidate.is_file():
            return candidate.resolve()

    dataset_id = str(document.get("dataset_id") or "").strip()
    file_name = str(document.get("file_name") or "").strip()
    if not dataset_id or Path(dataset_id).name != dataset_id or dataset_id in {".", ".."}:
        return None
    relative = Path(file_name)
    if not file_name or relative.anchor or ".." in relative.parts:
        return None
    storage = Path(content_dir) if content_dir is not None else mutable_path("storage/datasets")
    dataset_root = (storage / dataset_id).resolve()
    candidate = (dataset_root / relative).resolve()
    return candidate if candidate.is_relative_to(dataset_root) and candidate.is_file() else None
