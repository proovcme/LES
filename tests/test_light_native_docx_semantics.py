"""Native DOCX semantics: font encodings, list markers, textboxes and OMML structure."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import zipfile

import pytest

from backend import native_core


@pytest.fixture(autouse=True)
def native():
    if not os.getenv('LES_CORE_LIBRARY'):
        pytest.skip('Native conversion ABI required')
    assert native_core.supports('document_conversion')


def convert(tmp_path, body, numbering=None):
    path = tmp_path / 'Текст и формулы 東京.docx'
    with zipfile.ZipFile(path, 'w', zipfile.ZIP_DEFLATED) as archive:
        archive.writestr('word/document.xml',
            '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" '
            'xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" '
            'xmlns:v="urn:schemas-microsoft-com:vml"><w:body>' + body + '</w:body></w:document>')
        if numbering is not None:
            archive.writestr('word/numbering.xml', numbering)
    before = hashlib.sha256(path.read_bytes()).hexdigest()
    try:
        return native_core.required('document', request={
            'schema': 'les.document-convert.v1', 'action': 'convert', 'path': str(path.resolve())})
    finally:
        assert hashlib.sha256(path.read_bytes()).hexdigest() == before


@pytest.mark.parametrize('font,code,text', [
    ('Symbol', 'F02D', '−'), ('Symbol', 'F0B4', '×'), ('Symbol', 'F053', 'Σ'),
    ('Symbol', 'F067', 'γ'), ('symbol', '002d', '−'), ('Wingdings', 'F0A7', '▪'), ('Technic','F025','%'),
])
def test_font_bound_symbol_is_preserved(tmp_path, font, code, text):
    value = convert(tmp_path, f'<w:p><w:r><w:t>A</w:t><w:sym w:font="{font}" w:char="{code}"/><w:t>B</w:t></w:r></w:p>')
    assert value['text'] == f'A{text}B'


@pytest.mark.parametrize('font,code', [('Technic', 'F026'), ('Unknown', 'F02D'), ('Symbol', 'E02D')])
def test_unknown_glyph_does_not_get_guessed(tmp_path, font, code):
    with pytest.raises(native_core.NativeCoreError, match='^DOCX_SYMBOL_UNSUPPORTED$'):
        convert(tmp_path, f'<w:p><w:r><w:t>Readable</w:t><w:sym w:font="{font}" w:char="{code}"/></w:r></w:p>')


@pytest.mark.parametrize('font,marker,expected', [('Symbol', '\uf02d', '−'), ('Symbol', '\uf0b7', '•'),
    ('Wingdings', '\uf0a7', '▪'), ('Unknown', '\uf02d', None), ('', '\uf0b7', None)])
def test_private_use_list_marker_uses_declared_font(tmp_path, font, marker, expected):
    numbering = ('<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
        '<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/>'
        f'<w:numFmt w:val="bullet"/><w:lvlText w:val="{marker}"/>'
        f'<w:rPr><w:rFonts w:ascii="{font}"/></w:rPr></w:lvl></w:abstractNum>'
        '<w:num w:numId="1"><w:abstractNumId w:val="1"/></w:num></w:numbering>')
    paragraph = '<w:p><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Кабель</w:t></w:r></w:p>'
    if expected is None:
        with pytest.raises(native_core.NativeCoreError, match='^DOCX_NUMBERING_UNSUPPORTED$'):
            convert(tmp_path, paragraph, numbering)
    else:
        assert convert(tmp_path, paragraph, numbering)['text'] == expected + ' Кабель'


def term(text):
    return f'<m:r><m:t>{text}</m:t></m:r>'


def test_omml_quotient_and_exponents_keep_grouping_and_source_order(tmp_path):
    numerator = '<m:sSup><m:sSupPr><m:ctrlPr><w:rPr><w:sz w:val="24"/></w:rPr></m:ctrlPr></m:sSupPr><m:e>' + term('a+b') + '</m:e><m:sup>' + term('2') + '</m:sup></m:sSup>'
    formula = '<m:oMath><m:f><m:num>' + numerator + '</m:num><m:den>' + term('c−d') + '</m:den></m:f></m:oMath>'
    assert convert(tmp_path, '<w:p><w:r><w:t>Q=</w:t></w:r>' + formula + '<w:r><w:t> кВт</w:t></w:r></w:p>')['text'] == 'Q=((a+b)^(2))/(c−d) кВт'


@pytest.mark.parametrize('expression', [
    '<m:rad><m:e>' + term('x') + '</m:e></m:rad>',
    '<m:f><m:fPr><m:type m:val="noBar"/></m:fPr><m:num>' + term('a') + '</m:num><m:den>' + term('b') + '</m:den></m:f>',
    '<m:sSup><m:sup>' + term('2') + '</m:sup><m:e>' + term('a') + '</m:e></m:sSup>',
    '<m:sSup><m:e>' + term('a') + '</m:e></m:sSup>', '',
])
def test_unsupported_or_incomplete_equation_is_not_flattened(tmp_path, expression):
    with pytest.raises(native_core.NativeCoreError, match='^DOCX_EQUATION_UNSUPPORTED$'):
        convert(tmp_path, '<w:p><w:r><w:t>Text before failure</w:t></w:r><m:oMath>' + expression + '</m:oMath></w:p>')


def textbox(content):
    return '<w:pict><v:shape><v:textbox><w:txbxContent>' + content + '</w:txbxContent></v:textbox></v:shape></w:pict>'


def test_textbox_retains_paragraphs_table_cells_and_surrounding_text(tmp_path):
    box = textbox('<w:p><w:r><w:t>Внутри &amp; 東京</w:t></w:r></w:p>'
        '<w:tbl><w:tr><w:tc><w:p><w:r><w:t>25 кВт</w:t></w:r></w:p></w:tc></w:tr></w:tbl>')
    value = convert(tmp_path, '<w:p><w:r><w:t>До</w:t>' + box + '<w:t>После</w:t></w:r></w:p>')['text']
    assert value == 'До\n\nВнутри & 東京\n\n|  |\n| --- |\n| 25 кВт |\n\nПосле'


@pytest.mark.parametrize('extra', ['<v:imagedata r:id="hidden" xmlns:r="r"/>', '<w:object/>', '<v:unknown/>'])
def test_textbox_does_not_conceal_unread_content(tmp_path, extra):
    box = textbox('<w:p><w:r><w:t>Visible textbox</w:t></w:r></w:p>')
    box = box.replace('</v:shape>', extra + '</v:shape>')
    with pytest.raises(native_core.NativeCoreError, match='^DOCX_(DRAWING|EMBEDDED_OBJECT)_UNSUPPORTED$'):
        convert(tmp_path, '<w:p><w:r>' + box + '</w:r></w:p>')


def test_textbox_semantic_nesting_has_a_hard_bound(tmp_path):
    body = '<w:p><w:r><w:t>deep</w:t></w:r></w:p>'
    for _ in range(55):
        body = '<w:p><w:r>' + textbox(body) + '</w:r></w:p>'
    with pytest.raises(native_core.NativeCoreError, match='^DOCX_DRAWING_UNSUPPORTED$'):
        convert(tmp_path, body)


def test_textbox_story_list_does_not_borrow_body_counters(tmp_path):
    box = textbox('<w:p><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr>'
        '<w:r><w:t>Independent story</w:t></w:r></w:p>')
    with pytest.raises(native_core.NativeCoreError, match='^DOCX_DRAWING_UNSUPPORTED$'):
        convert(tmp_path, '<w:p><w:r>' + box + '</w:r></w:p>')


def test_table_grid_omissions_keep_columns_and_vertical_merge_alignment(tmp_path):
    cell = lambda text, props='': '<w:tc><w:tcPr>' + props + '</w:tcPr><w:p><w:r><w:t>' + text + '</w:t></w:r></w:p></w:tc>'
    table = ('<w:tbl><w:tblGrid><w:gridCol/><w:gridCol/><w:gridCol/></w:tblGrid>'
        '<w:tr>' + cell('A') + cell('B', '<w:vMerge w:val="restart"/>') + cell('C') + '</w:tr>'
        '<w:tr><w:trPr><w:gridBefore w:val="1"/><w:gridAfter w:val="1"/></w:trPr>'
        + cell('', '<w:vMerge/>') + '</w:tr></w:tbl>')
    assert convert(tmp_path, table)['text'].endswith('| A | B | C |\n|  | B |  |')


def test_missing_table_columns_are_not_guessed_and_not_blamed_on_a_list(tmp_path):
    body = ('<w:p><w:pPr><w:numPr><w:numId w:val="0"/></w:numPr></w:pPr>'
        '<w:r><w:t>Ordinary text</w:t></w:r></w:p>'
        '<w:tbl><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid><w:tr><w:tc>'
        '<w:p><w:r><w:t>Only one unmerged cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl>')
    with pytest.raises(native_core.NativeCoreError, match='^DOCX_TABLE_UNSUPPORTED$'):
        convert(tmp_path, body)


def test_formula_success_does_not_mask_later_drawing_failure(tmp_path):
    body = '<w:p><m:oMath>' + term('x=25') + '</m:oMath><w:r><w:drawing/></w:r></w:p>'
    with pytest.raises(native_core.NativeCoreError, match='^DOCX_DRAWING_UNSUPPORTED$'):
        convert(tmp_path, body)


def test_textbox_attribute_entities_cannot_inject_normalized_markup(tmp_path):
    body = textbox('<w:p><w:hyperlink w:tooltip="&quot;/&gt;&lt;w:object/&gt;">'
        '<w:r><w:t>Safe &amp; unchanged</w:t></w:r></w:hyperlink></w:p>')
    assert convert(tmp_path, '<w:p><w:r>' + body + '</w:r></w:p>')['text'] == 'Safe & unchanged'


def test_duplicate_docx_members_never_select_an_arbitrary_revision(tmp_path):
    path = tmp_path / 'duplicate.docx'
    with pytest.warns(UserWarning, match='Duplicate name'):
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('word/document.xml', '<wrong/>')
            archive.writestr('word/document.xml', '<other/>')
    with pytest.raises(native_core.NativeCoreError, match='^DOCUMENT_ARCHIVE_INVALID$'):
        native_core.required('document', request={'schema': 'les.document-convert.v1', 'action': 'convert', 'path': str(path.resolve())})


def test_docx_worker_has_no_python_dependency_and_recovers_after_error(tmp_path):
    api = os.getenv('LES_TEST_NATIVE_API')
    if not api:
        pytest.skip('Native executable required')
    expected = convert(tmp_path, '<w:p><w:r>' + textbox('<w:p><w:r><w:t>Proof</w:t></w:r></w:p>') +
        '</w:r><m:oMath>' + term('x=25') + '</m:oMath></w:p>')['text']
    good = tmp_path / 'Текст и формулы 東京.docx'
    bad = tmp_path / 'broken.docx'
    bad.write_bytes(b'not a ZIP')
    environment = {**os.environ, 'PATH': '', 'PYTHONHOME': str(tmp_path / 'missing'),
        'PYTHONPATH': str(tmp_path / 'missing')}
    frames = ''.join(json.dumps({'request_id': i, 'input': {'schema': 'les.document-convert.v1',
        'path': str(path.resolve()), 'owned_state':str(tmp_path.resolve())}}) + '\n' for i, path in enumerate((good, bad, good)))
    result = subprocess.run([str(Path(api).resolve()), '--serve-document-formats'], input=frames,
        text=True, encoding='utf-8', capture_output=True, env=environment, timeout=20)
    assert result.returncode == 0
    rows = [json.loads(line)['result'] for line in result.stdout.splitlines()]
    assert [row['status'] for row in rows] == ['completed', 'failed', 'completed']
    assert rows[1]['code'] == 'DOCUMENT_ARCHIVE_INVALID'
    assert rows[0]['value']['text'] == rows[2]['value']['text'] == expected
    assert rows[0]['value']['owner'] == 'rust.document.conversion'
