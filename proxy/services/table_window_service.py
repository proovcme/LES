"""Classic codec for native addressed reads of immutable table snapshots."""
from proxy.services.document_task_store import native_request


def read_window(document, *, sheet, row_start=1, column_start=1, row_count=4, column_count=8, max_bytes=6000):
    return native_request('window', dict(document=document, operation='read_window', sheet=sheet,
        row_start=row_start, column_start=column_start, row_count=row_count,
        column_count=column_count, max_bytes=max_bytes))


def read_cell(document, *, sheet, coordinate, field='value', offset=0, max_chars=1000):
    return native_request('window', dict(document=document, operation='read_cell', sheet=sheet,
        coordinate=coordinate, field=field, offset=offset, max_chars=max_chars))
