from openpyxl import Workbook
import pytest

from proxy.services.tabular_document_service import read_table, encode
from proxy.services.table_window_service import read_window, read_cell


def table(tmp_path):
    path = tmp_path/'Окно.xlsx'
    book = Workbook(); sheet = book.active; sheet.title = 'Лист с пробелами'
    sheet.append([0, False, '0012', None, '=A1+1'])
    sheet['I1'] = 'Конец строки'
    sheet['A2'] = 'Я😀' * 9000
    sheet.merge_cells('B2:D2'); sheet['B2'] = 'Общий заголовок'
    sheet['A6'] = 'Последняя строка'
    book.save(path); book.close()
    return read_table(path)


def test_window_pagination_covers_coordinates_once_without_clipping_long_cells(tmp_path):
    document = table(tmp_path)
    cursor, seen, cells = {}, [], {}
    while True:
        page = read_window(document, sheet=document['sheets'][0]['name'], **cursor)
        assert len(encode(page).encode('utf-8')) <= 6000
        for cell in page['cells']:
            seen.append(cell['coordinate']); cells[cell['coordinate']] = cell
        if page['next_cursor'] is None:
            break
        cursor = page['next_cursor']
    assert len(seen) == len(set(seen)) == 54
    assert cells['A1']['value'] == '0' and cells['B1']['value'] is False
    assert cells['C1']['value'] == '0012' and cells['D1']['kind'] == 'blank_or_absent'
    assert cells['A2']['deferred'] is True
    assert cells['C2'] == {'coordinate': 'C2', 'kind': 'merged_continuation', 'anchor': 'B2'}
    assert cells['E1']['formula'] == '=A1+1' and cells['E1']['cached'] is None


def test_long_cell_char_cursors_and_formula_cache_are_literal(tmp_path):
    document = table(tmp_path); sheet = document['sheets'][0]['name']
    pieces, offset = [], 0
    while True:
        page = read_cell(document, sheet=sheet, coordinate='A2', offset=offset, max_chars=301)
        pieces.append(page['text'])
        if page['next_offset'] is None:
            break
        offset = page['next_offset']
    assert ''.join(pieces) == 'Я😀' * 9000
    assert read_cell(document, sheet=sheet, coordinate='E1', field='cached')['text'] == 'null'
    with pytest.raises(ValueError):
        read_cell(document, sheet=sheet, coordinate='E1', field='value')
    with pytest.raises(ValueError):
        read_window(document, sheet=sheet, row_start=True)
