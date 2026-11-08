"""Native saved values for XLSX classification and structured indexing."""
import logging
from pathlib import Path

from backend import native_core

logger = logging.getLogger(__name__)
_COMPATIBILITY = {
    "WORKBOOK_COMPATIBILITY", "WORKBOOK_ARCHIVE_LIMIT", "WORKBOOK_FILE_LIMIT",
    "CORE_OUTPUT_LIMIT",
}


def read(path: Path, *, action: str) -> dict | None:
    from backend.native_document_conversion import read as convert
    if path.suffix.lower() == '.xls' and action == 'read':
        return convert(path, view='tables', read_rows=20_000)
    if path.suffix.lower() not in {".xlsx", ".xlsm"} or not native_core.supports("workbook_index"):
        return None
    try:
        return native_core.required("workbook_index", request={"action": action, "path": str(path)})
    except native_core.NativeCoreError as error:
        if str(error) not in _COMPATIBILITY:
            raise
        logger.info("Excel %s uses compatibility reader: %s", action, error)
        if action == 'read':
            return convert(path, view='tables', read_rows=20_000)
        return None
