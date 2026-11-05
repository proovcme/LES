"""Addressed reads of immutable table snapshots; long values have explicit cursors."""
from __future__ import annotations

from openpyxl.utils import get_column_letter
from openpyxl.worksheet.cell_range import CellRange

from proxy.services.tabular_document_service import encode


def _sheet(document, sheet_name):
    sheet = next((item for item in document['sheets'] if item['name'] == sheet_name), None)
    if sheet is None:
        raise ValueError('Неизвестный лист; используйте точное имя из списка sheets')
    return sheet


def read_window(document, *, sheet, row_start=1, column_start=1, row_count=4, column_count=8, max_bytes=6000):
    metadata = _sheet(document, sheet)
    if (any(type(value) is not int or value < 1 for value in (row_start, column_start, row_count, column_count)) or
            row_count > 4 or column_count > 8 or row_start > 20000 or column_start > 512 or
            not 512 <= max_bytes <= 12000):
        raise ValueError('Нужно окно до 4 строк и 8 столбцов в пределах таблицы')
    rows = {item['row']: item for item in document['rows'] if item['sheet'] == sheet}
    height = max(rows, default=0)
    if row_start > height or column_start > metadata['columns']:
        raise ValueError('Окно за пределами непустой области таблицы')
    last_row = min(height, row_start + row_count - 1)
    last_column = min(metadata['columns'], column_start + column_count - 1)
    merges = [CellRange(item) for item in metadata['merged_ranges']]
    cells = []
    allowance = max(64, (max_bytes - 500) // ((last_row-row_start+1) * (last_column-column_start+1)))
    for number in range(row_start, last_row + 1):
        by_coordinate = {item['coordinate']: item for item in rows.get(number, {}).get('cells', [])}
        for column in range(column_start, last_column + 1):
            coordinate = f'{get_column_letter(column)}{number}'
            cell = by_coordinate.get(coordinate, {'coordinate': coordinate, 'kind': 'blank_or_absent'})
            merged = next((item for item in merges if coordinate in item), None)
            anchor = f'{get_column_letter(merged.min_col)}{merged.min_row}' if merged else None
            if merged and coordinate != anchor:
                cell = {'coordinate': coordinate, 'kind': 'merged_continuation', 'anchor': anchor}
            if len(encode(cell).encode('utf-8')) > allowance:
                cell = {'coordinate': coordinate, 'kind': cell['kind'], 'deferred': True, 'read': 'read_cell'}
            cells.append(cell)
    next_cursor = ({'row_start': row_start, 'column_start': last_column + 1} if last_column < metadata['columns'] else
                   {'row_start': last_row + 1, 'column_start': 1} if last_row < height else None)
    result = {'sheet': sheet, 'source_sha256': document['sha256'], 'cells': cells,
            'bounds': [row_start, column_start, last_row, last_column], 'next_cursor': next_cursor,
            'read_advances_review': False, 'blank_policy': 'Snapshot does not distinguish absent and explicit empty cells.'}
    if len(encode(result).encode('utf-8')) > max_bytes:
        raise ValueError('Окно превышает бюджет; уменьшите row_count или column_count')
    return result


def read_cell(document, *, sheet, coordinate, field='value', offset=0, max_chars=1000):
    _sheet(document, sheet)
    if (type(offset) is not int or offset < 0 or type(max_chars) is not int or not 128 <= max_chars <= 3000 or
            field not in {'value', 'formula', 'cached'}):
        raise ValueError('Некорректная часть ячейки')
    cell = next((cell for row in document['rows'] if row['sheet'] == sheet for cell in row['cells']
                 if cell['coordinate'] == coordinate), None)
    if cell is None:
        raise ValueError('Нет сохранённого значения этой ячейки; проверьте окно и объединения')
    if field not in cell:
        raise ValueError('В ячейке нет выбранного поля; для формулы используйте formula или cached')
    value = cell[field]
    text = value if isinstance(value, str) else encode(value)
    if offset > len(text) or (offset == len(text) and offset != 0):
        raise ValueError('Смещение за пределами значения ячейки')
    stop = min(len(text), offset + max_chars)
    return {'sheet': sheet, 'coordinate': coordinate, 'kind': cell['kind'], 'field': field,
            'source_sha256': document['sha256'], 'offset': offset, 'text': text[offset:stop],
            'representation': 'literal_text' if isinstance(value, str) else 'json',
            'next_offset': stop if stop < len(text) else None, 'total_chars': len(text),
            'read_advances_review': False}
