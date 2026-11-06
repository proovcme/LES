"""Exercise the native snapshot at the existing chat boundary."""
from datetime import datetime
from zipfile import ZIP_DEFLATED, ZipFile

import openpyxl
import pytest

from backend import native_core
from proxy.services import tabular_document_service as tables


@pytest.fixture(autouse=True)
def require_native_workbook(monkeypatch):
    if not native_core.supports('table_snapshot'):
        pytest.skip('Build the current Rust core to verify workbook snapshots')
    monkeypatch.setenv('LES_CORE_MODE', 'native')


def rewrite_sheet(source, target, edit):
    with ZipFile(source) as before, ZipFile(target, 'w', compression=ZIP_DEFLATED) as after:
        for entry in before.infolist():
            data = before.read(entry)
            if entry.filename == 'xl/worksheets/sheet1.xml':
                data = edit(data)
            after.writestr(entry.filename, data)


@pytest.mark.parametrize('suffix', ['.xlsx', '.xlsm'])
def test_native_reader_preserves_originals_and_formula_caches(tmp_path, monkeypatch, suffix):
    source = tmp_path / 'base.xlsx'
    target = tmp_path / ('Книга с пробелами 🌲' + suffix)
    book = openpyxl.Workbook()
    sheet = book.active
    sheet.title = "Лист '1'"
    sheet.append(['Монтаж & проверка\nВторая строка', 0, False, '=B1+2', '=B1/0'])
    sheet.append(['=B2+2', None, None, '=A2'])
    sheet['A3'] = '=B3+2'
    sheet.merge_cells('A4:B4')
    sheet['A4'] = 'Заголовок'
    hidden = book.create_sheet('Скрытый лист')
    hidden.sheet_state = 'veryHidden'
    hidden['D500'] = 0.125
    book.save(source)
    book.close()

    def edit(xml):
        return (xml.replace(b'<f>B1+2</f><v></v>', b'<f>B1+2</f><v>2</v>')
                .replace(b'r="E1"><f>B1/0</f><v></v>', b'r="E1" t="e"><f>B1/0</f><v>#DIV/0!</v>')
                .replace(b'<f>B2+2</f>', b'<f t="shared" si="0" ref="A2:A3">B2+2</f>')
                .replace(b'<f>B3+2</f>', b'<f t="shared" si="0"/>'))

    rewrite_sheet(source, target, edit)
    original = target.read_bytes()
    from proxy.services.file_viewer_service import file_viewer_html

    preview = file_viewer_html(target, path_id="test", locator=f"{sheet.title}!R1")
    assert '<td>2<br><small class="formula">=B1+2</small></td>' in preview
    with monkeypatch.context() as legacy:
        legacy.setenv('LES_CORE_MODE', 'python')
        expected = tables.read_table(target)

    def fail(*_args, **_kwargs):
        pytest.fail('Ordinary workbook called openpyxl instead of Rust')

    monkeypatch.setattr(openpyxl, 'load_workbook', fail)
    actual = tables.read_table(target)
    assert actual == expected
    assert actual['rows'][0]['cells'][3]['cached']['value'] == '2'
    assert actual['rows'][0]['cells'][4]['cached']['kind'] == 'error'
    assert actual['rows'][2]['cells'][0]['formula'] == '=B3+2'
    assert actual['rows'][-1]['id'] == 's2:r500'
    assert target.read_bytes() == original


def test_compatibility_preserves_calendar_and_integer_precision(tmp_path, monkeypatch):
    source = tmp_path / 'base.xlsx'
    target = tmp_path / 'precision.xlsx'
    book = openpyxl.Workbook()
    book.active['A1'] = 7
    book.active['A2'] = datetime(2026, 10, 3, 10, 20, 30)
    book.save(source)
    book.close()
    rewrite_sheet(source, target, lambda xml: xml.replace(b'<v>7</v>', b'<v>9007199254740993</v>'))
    with pytest.raises(native_core.NativeCoreError, match='WORKBOOK_COMPATIBILITY'):
        native_core.required('table_snapshot', request={'path': str(target)})
    document = tables.read_table(target)
    assert document['rows'][0]['cells'][0]['value'] == '9007199254740993'
    assert document['rows'][1]['cells'][0]['value'] == '2026-10-03T10:20:30'


def test_native_reader_rejects_wide_sheet_and_expanded_archive(tmp_path, monkeypatch):
    source = tmp_path / 'wide.xlsx'
    book = openpyxl.Workbook()
    book.active.cell(row=1, column=513, value=7)
    book.save(source)
    book.close()
    with pytest.raises(ValueError, match='предел строк или столбцов'):
        tables.read_table(source)

    with ZipFile(source, 'a', compression=ZIP_DEFLATED) as archive:
        with archive.open('xl/oversize.bin', 'w') as entry:
            for _ in range(64):
                entry.write(b'\0' * (1024 * 1024))
    with pytest.raises(ValueError, match='Распакованная таблица превышает 64 МиБ'):
        tables.read_table(source)


def test_index_routing_and_nodes_match_previous_reader(tmp_path, monkeypatch):
    from dataclasses import asdict

    from backend import qdrant_support as support
    from backend.document_router import probe_document
    from backend.parquet_writer import read_xlsx_sheets
    from backend.qdrant_ingestion import QdrantIngestion
    from backend.qdrant_nodes import QdrantNodes

    if not native_core.supports('workbook_index'):
        pytest.skip('Build the current core to verify native Excel indexing')
    source = tmp_path / 'Рабочая книга.xlsx'
    path = tmp_path / 'Таблица с пробелами 日本語.xlsx'
    book = openpyxl.Workbook()
    sheet = book.active
    sheet.title = 'Щиты'
    sheet['B2'] = 'Ведомость'
    sheet.merge_cells('B2:D2')
    sheet.append(['Позиция', 'Наименование', 'Количество', 'Единица', 'Примечание'])
    sheet['A5'], sheet['B5'], sheet['C5'], sheet['D5'], sheet['E5'] = 'ЩР-01', 'Щит силовой', '=1-1', 'шт', False
    sheet['A8'], sheet['B8'], sheet['C8'], sheet['D8'] = 'ЩР-02', 'Щит распределительный', 1.25, 'шт'
    extra = book.create_sheet('Дополнение')
    extra.sheet_state = 'hidden'
    extra.append(['Позиция', 'Наименование', 'Количество', 'Единица'])
    extra.append(['КЛ-1', 'Кабель силовой', 16, 'м'])
    book.save(source)
    book.close()
    rewrite_sheet(source, path, lambda xml: xml.replace(b'<f>1-1</f><v></v>', b'<f>1-1</f><v>0</v>'))
    original = path.read_bytes()
    splitter = support.StructureAwareSplitter(chunk_size=1000, chunk_overlap=100)
    data = tmp_path / 'state'
    data.mkdir()

    def index():
        return QdrantIngestion._convert_file_inline(
            QdrantNodes(), path, data, path.name, 'fixture', support.MarkdownNodeParser(),
            splitter, {}, False,
        )

    with monkeypatch.context() as legacy:
        legacy.setenv('LES_CORE_MODE', 'python')
        expected_probe = asdict(probe_document(path))
        expected_sheets = read_xlsx_sheets(str(path))
        expected_route, expected_nodes = index()

    def fail(*_args, **_kwargs):
        pytest.fail('Excel indexing called openpyxl instead of Rust')

    monkeypatch.setattr(openpyxl, 'load_workbook', fail)
    assert asdict(probe_document(path)) == expected_probe
    actual = read_xlsx_sheets(str(path))
    assert actual == expected_sheets
    assert [sheet['header_row'] for sheet in actual] == [3, 1]
    assert [sheet['row_numbers'] for sheet in actual] == [[5, 8], [2]]
    assert actual[0]['rows'][0]['Количество'] == 0
    assert actual[0]['rows'][0]['Примечание'] is False
    route, nodes = index()
    assert asdict(route) == asdict(expected_route)
    assert len(nodes) == len(expected_nodes) == 3
    assert all(item['payload']['type'] == 'table_row' for item in nodes)
    assert [(item['payload']['sheet_name'], item['payload']['row']) for item in nodes] == [
        ('Щиты', 5), ('Щиты', 8), ('Дополнение', 2)]
    assert nodes == expected_nodes
    assert path.read_bytes() == original


@pytest.mark.parametrize(('column', 'content'), [
    (2, datetime(2026, 10, 3, 10, 20, 30)),
    (513, 'За пределом нативного окна'),
])
def test_index_compatibility_keeps_dates_and_large_sheets(tmp_path, column, content):
    from backend.parquet_writer import read_xlsx_sheets

    if not native_core.supports('workbook_index'):
        pytest.skip('Build the current core to verify native Excel indexing')
    path = tmp_path / 'Календарь.xlsx'
    book = openpyxl.Workbook()
    sheet = book.active
    sheet['A1'] = 'Событие'
    sheet['A2'] = 'Проверка'
    sheet.cell(row=1, column=column, value='Контроль')
    sheet.cell(row=2, column=column, value=content)
    book.save(path)
    book.close()
    with pytest.raises(native_core.NativeCoreError, match='WORKBOOK_COMPATIBILITY'):
        native_core.required('workbook_index', request={'action': 'read', 'path': str(path)})
    assert read_xlsx_sheets(str(path))[0]['rows'][0]['Контроль'] == content
