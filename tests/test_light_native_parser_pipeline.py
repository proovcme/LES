"""Real native Docling/Xberg workers: identity, special content and empty publication gate."""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import zipfile

import pytest
from docx import Document
from docx.oxml import parse_xml
from tests.pdf_fixtures import write_pdf
from tests.test_light_native_pdf_layout import grid


@pytest.fixture
def converter(tmp_path):
    api=os.getenv('LES_TEST_NATIVE_API');parsers=os.getenv('LES_TEST_NATIVE_DOCUMENT_PARSERS')
    if not api or not parsers:pytest.skip('Build native API and native document parser assets')
    import pypdfium2_raw
    library=Path(pypdfium2_raw.__file__).with_name('pdfium.dll')
    state=tmp_path/'owned';state.mkdir()
    env={**os.environ,'LES_NATIVE_DOCUMENT_PARSERS':str(Path(parsers).resolve()),
         'LES_STATE_ROOT':str(state),'LES_NATIVE_PDFIUM':str(library),
         'PYTHONHOME':str(tmp_path/'no-python'),'PYTHONPATH':str(tmp_path/'no-application')}
    def call(path,*,pdf=False,**extra):
        source=hashlib.sha256(path.read_bytes()).hexdigest()
        request={'schema':'les.document-convert.v1','action':'convert','path':str(path.resolve()),
                 'owned_state':str(state),'source_sha256':source,'ocr_unavailable':True,**extra}
        args=['--serve-pdf-text',str(library)] if pdf else ['--serve-document-formats']
        process=subprocess.run([api,*args],input=json.dumps({'request_id':1,'input':request})+'\n',
            env=env,capture_output=True,text=True,encoding='utf-8',timeout=35)
        assert process.returncode==0,process.stderr
        assert hashlib.sha256(path.read_bytes()).hexdigest()==source
        return json.loads(process.stdout)['result']
    call.state=state
    return call


def word(path,math=False):
    doc=Document();doc.add_heading('1. Требования Ω',1)
    doc.add_paragraph('Ёлка 日本語 — пробелы и значения 17,25 мм²')
    doc.add_paragraph('FIRST ITEM',style='List Number')
    table=doc.add_table(rows=2,cols=2)
    for row,values in zip(table.rows,[['Position','Quantity'],['Alpha item','17.25']]):
        for cell,text in zip(row.cells,values):cell.text=text
    header=doc.sections[0].header;header.paragraphs[0].text='HEADER PLAIN'
    header._element.append(parse_xml('''<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:v="urn:schemas-microsoft-com:vml"><w:r><w:pict><v:shape><v:textbox><w:txbxContent><w:p><w:r><w:t>HEADER TEXTBOX ANCHOR</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p>'''))
    if math:
        doc._element.body.insert(1,parse_xml('''<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:oMath><m:f><m:num><m:r><m:t>17.25</m:t></m:r></m:num><m:den><m:r><m:t>3</m:t></m:r></m:den></m:f></m:oMath></w:p>'''))
    doc.add_paragraph('FINAL SOURCE TAIL');doc.save(path)
    return path


def test_docling_word_preserves_header_textbox_math_and_original_part_identity(converter,tmp_path):
    path=word(tmp_path/'Word with spaces Ω.docx',math=True)
    response=converter(path);assert response['status']=='completed',response
    value=response['value'];text=value['text']
    for term in ['Требования Ω','日本語','17,25','Alpha item','17.25','HEADER PLAIN','HEADER TEXTBOX ANCHOR','FINAL SOURCE TAIL','(17.25)/(3)']:
        assert term in text,term
    assert 'LESPART' not in text
    assert value['metadata']['extractor']=='les.docling-word.parts.v1'
    sha=hashlib.sha256(path.read_bytes()).hexdigest()
    assert value['metadata']['docx_parts_source_sha256']==sha
    root=converter.state/'storage/docx-parts'/sha
    assert (root/sha).read_bytes()==path.read_bytes()


def test_empty_word_does_not_become_indexed_nodes(converter,tmp_path):
    path=tmp_path/'Empty.docx';Document().save(path)
    value=converter(path,pipeline='nodes',data_dir=str(converter.state/'data'),dataset_id='ds',file_key=path.name,
                    chunking={'unit':'chars','chunk_size':800,'chunk_overlap':80},minimum=1,final_minimum=1)
    assert value['status']=='failed' and value['code']=='NO_SEARCHABLE_TEXT'
    assert 'nodes' not in value and not list(converter.state.rglob('*.tmp'))


def test_checked_math_in_a_merged_word_cell_uses_span_proof(converter,tmp_path):
    path=word(tmp_path/'Merged formula.docx');doc=Document(path)
    table=doc.add_table(rows=2,cols=2);table.cell(0,0).text='Formula';table.cell(0,1).text='Context'
    cell=table.cell(1,0).merge(table.cell(1,1));cell.text='Merged source '
    cell.paragraphs[0]._p.append(parse_xml('''<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:f><m:num><m:r><m:t>17.25</m:t></m:r></m:num><m:den><m:r><m:t>3</m:t></m:r></m:den></m:f></m:oMath>'''))
    doc.save(path);value=converter(path);assert value['status']=='completed',value
    text=value['value']['text'];assert text.count('(17.25)/(3)')==2
    assert 'LESPART' not in text


def test_standard_symbol_font_text_is_decoded_by_docling(converter,tmp_path):
    path=word(tmp_path/'Symbol font.docx');doc=Document(path)
    doc._element.body.insert(1,parse_xml('''<w:p xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:r><w:rPr><w:rFonts w:ascii="Symbol" w:hAnsi="Symbol"/></w:rPr><w:t>\uf0d7</w:t></w:r></w:p>'''))
    doc.save(path);value=converter(path);assert value['status']=='completed',value
    assert '⋅' in value['value']['text'] and '\uf0d7' not in value['value']['text']


@pytest.mark.parametrize('columns',[2,3,7,17])
def test_merged_header_cell_uses_canonical_geometry_despite_flat_export(converter,tmp_path,columns):
    from docx.shared import Inches
    path=word(tmp_path/'Header table.docx');doc=Document(path)
    table=doc.sections[0].header.add_table(rows=2,cols=columns,width=Inches(6))
    for i,cell in enumerate(table.rows[0].cells):cell.text=f'Column {i+1}'
    cell=table.cell(1,0).merge(table.cell(1,columns-1));cell.text='Unified source '
    cell.paragraphs[0]._p.append(parse_xml('''<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:f><m:num><m:r><m:t>17.25</m:t></m:r></m:num><m:den><m:r><m:t>3</m:t></m:r></m:den></m:f></m:oMath>'''))
    doc.save(path);value=converter(path);assert value['status']=='completed',value
    assert value['value']['text'].count('(17.25)/(3)')==columns


@pytest.mark.parametrize('columns',[2,3,7,17])
def test_nested_table_span_cannot_multiply_unmerged_outer_cell(converter,tmp_path,columns):
    path=word(tmp_path/'Nested grid.docx');doc=Document(path)
    outer=doc.add_table(rows=1,cols=1);cell=outer.cell(0,0);cell.text='OUTER SINGLE '
    cell.paragraphs[0]._p.append(parse_xml('''<m:oMath xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math"><m:r><m:t>91.125</m:t></m:r></m:oMath>'''))
    inner=cell.add_table(rows=2,cols=columns)
    for i,c in enumerate(inner.rows[0].cells):c.text=f'Column {i}'
    inner.cell(1,0).merge(inner.cell(1,columns-1)).text='INNER MERGED'
    cell.add_paragraph('OUTER TAIL');doc.save(path)
    value=converter(path);assert value['status']=='completed',value
    text=value['value']['text'];assert text.count('OUTER SINGLE')==1
    assert text.count('91.125')==1 and text.count('OUTER TAIL')==1
    assert 'INNER MERGED' in text and 'LESPART' not in text


@pytest.mark.parametrize('properties',['absent','empty'])
def test_nested_vertical_merge_cannot_copy_a_previous_outer_row(converter,tmp_path,properties):
    from docx.oxml import OxmlElement
    path=word(tmp_path/'Nested vertical.docx');doc=Document(path)
    outer=doc.add_table(rows=2,cols=1);outer.cell(0,0).text='PREVIOUS OUTER'
    cell=outer.cell(1,0);cell.text='CURRENT OUTER'
    tcpr=cell._tc.tcPr;cell._tc.remove(tcpr)
    if properties=='empty':cell._tc.insert(0,OxmlElement('w:tcPr'))
    inner=cell.add_table(rows=2,cols=2)
    inner.cell(0,0).merge(inner.cell(1,0)).text='VERTICAL INNER'
    inner.cell(0,1).text='Upper';inner.cell(1,1).text='Lower'
    cell.add_paragraph('CURRENT TAIL');doc.save(path)
    value=converter(path);assert value['status']=='completed',value
    text=value['value']['text']
    for term in ['PREVIOUS OUTER','CURRENT OUTER','CURRENT TAIL']:assert text.count(term)==1
    assert 'VERTICAL INNER' in text


@pytest.mark.skipif(os.name!='nt',reason='Windows OCR adapter')
def test_docling_preserves_multiline_ocr_inside_its_original_table_cell(converter,tmp_path):
    from PIL import Image
    import io
    helper=tmp_path/'ocr.ps1'
    helper.write_text('param([string]$ImagePath)\nWrite-Output \'{"text":"Alpha line\\nBeta | 42 <item>"}\'\n',encoding='utf8')
    image=io.BytesIO();Image.new('RGB',(32,32),'red').save(image,format='PNG');image.seek(0)
    path=tmp_path/'OCR table.docx';doc=Document();table=doc.add_table(rows=2,cols=2)
    table.cell(0,0).text='Position';table.cell(0,1).text='Drawing'
    table.cell(1,0).text='Alpha';table.cell(1,1).paragraphs[0].add_run().add_picture(image)
    doc.save(path);scratch=tmp_path/'ocr-scratch';scratch.mkdir()
    value=converter(path,ocr_unavailable=False,ocr={'engine':'windows','helper':str(helper.resolve()),'scratch':str(scratch.resolve()),'dpi':150})
    assert value['status']=='completed',value
    text=value['value']['text'];line=next(s for s in text.splitlines() if 'Alpha line' in s)
    assert 'Alpha line<br>Beta &#124; 42 &lt;item&gt;' in line
    assert line.count('|')==3 and value['value']['metadata']['ocr'] is True
    assert not list(scratch.iterdir()) and 'LESPART' not in text


def test_duplicate_archive_is_rejected_before_docling(converter,tmp_path):
    path=word(tmp_path/'Duplicate.docx')
    import warnings
    with zipfile.ZipFile(path,'a') as z,warnings.catch_warnings():
        warnings.simplefilter('ignore');z.writestr('word/document.xml','<wrong/>')
    value=converter(path)
    assert value['status']=='failed' and value['code']=='DOCUMENT_ARCHIVE_INVALID'


@pytest.mark.parametrize('transform',[False,True])
def test_xberg_table_cells_survive_same_page_read_and_version_check(converter,tmp_path,transform):
    path=grid(tmp_path/'schedule.pdf',transform=transform,continuation=True)
    value=converter(path);assert value['status']=='completed',value
    text=value['value']['metadata']['pdf_page_views'][0]['text']
    assert '| Alpha item continued description | pcs | 17.25 |' in text
    assert '| Beta item | m | 23.50 |' in text
    db=converter.state/'meta.db'
    with sqlite3.connect(db) as c:
        c.execute('CREATE TABLE documents(id TEXT,dataset_id TEXT,file_name TEXT,source_path TEXT,file_hash TEXT)')
        c.execute('INSERT INTO documents VALUES(?,?,?,?,?)',('actual-id','ds',path.name,str(path),hashlib.sha256(path.read_bytes()).hexdigest()))
    result=converter(path,pdf=True,kind='source_page',database=str(db),source={'doc_id':'actual-id','page':1})
    assert result['status']=='completed',result
    assert result['value']['text']==text
    assert result['value']['page']==1 and result['value']['doc_id']=='actual-id'
    assert result['value']['extractor']=='les.pdfium.xberg-page.v1'
    old=hashlib.sha256(path.read_bytes()).hexdigest();grid(path,quantity='99.99')
    changed=converter(path,pdf=True,kind='source_page',database=str(db),source={'doc_id':'actual-id','page':1,'source_sha256':old})
    assert changed['status']=='failed' and changed['code']=='PDF_REVISION_CHANGED'


def test_pdf_without_text_requires_ocr_and_publishes_no_empty_nodes(converter,tmp_path):
    path=write_pdf(tmp_path/'No text.pdf',[b'0 0 0 rg 20 20 180 180 re f'])
    value=converter(path,pipeline='nodes',data_dir=str(converter.state/'data'),dataset_id='ds',file_key=path.name,
                    chunking={'unit':'chars','chunk_size':800,'chunk_overlap':80},minimum=1,final_minimum=1)
    assert value['status']=='failed' and value['code']=='OCR_UNAVAILABLE'
    assert 'nodes' not in value


def test_pdfium_discretionary_hyphen_is_read_as_a_source_hyphen(converter,tmp_path):
    path=write_pdf(tmp_path/'Hyphenated source.pdf',[b'BT /F1 12 Tf 25 220 Td (Electrical-) Tj 0 -18 Td (equipment and 17.25) Tj ET'])
    result=converter(path);assert result['status']=='completed',result
    text=result['value']['text']
    assert '\ufffe' not in text and 'Electrical-' in text and 'equipment and 17.25' in text


def test_cold_pdf_reader_extracts_only_physical_second_page_and_keeps_paging(converter,tmp_path):
    import re
    original=grid(tmp_path/'grid.pdf',continuation=True)
    stream=re.search(rb'stream\r?\n(.*?)\r?\nendstream',original.read_bytes(),re.S).group(1)
    path=write_pdf(tmp_path/'Two physical pages.pdf',[b'BT /F1 12 Tf 30 220 Td (FIRST PAGE CONTENT MUST NOT LEAK) Tj ET',stream])
    db=converter.state/'meta.db';sha=hashlib.sha256(path.read_bytes()).hexdigest()
    with sqlite3.connect(db) as c:
        c.execute('CREATE TABLE documents(id TEXT,dataset_id TEXT,file_name TEXT,source_path TEXT,file_hash TEXT)')
        c.execute('INSERT INTO documents VALUES(?,?,?,?,?)',('actual-id','ds',path.name,str(path),sha))
    parts=[];offset=0
    while True:
        answer=converter(path,pdf=True,kind='source_page',database=str(db),source={'doc_id':'actual-id','page':2,'offset':offset,'max_chars':80})
        assert answer['status']=='completed',answer
        value=answer['value'];assert value['page']==2 and value['page_count']==2 and value['source_sha256']==sha
        parts.append(value['text'])
        if value['next_offset'] is None:break
        assert value['next_offset']>offset;offset=value['next_offset']
    text=''.join(parts);assert 'FIRST PAGE' not in text
    assert '| Alpha item continued description | pcs | 17.25 |' in text
    caches=list((converter.state/'storage/parser-tables').glob('*-p2.json'));assert len(caches)==1
    assert json.loads(caches[0].read_text())['physical_page']==2


def test_excel_values_and_missing_formula_cache_still_use_existing_reader(converter,tmp_path):
    from openpyxl import Workbook
    import xml.etree.ElementTree as ET
    path=tmp_path/'Values and formulas.xlsx';book=Workbook();sheet=book.active
    sheet.append(['Item','Value','Formula']);sheet.append(['Alpha',17.25,'=2+3']);sheet.append(['Beta',23.50,'=A99+1'])
    book.save(path)
    with zipfile.ZipFile(path) as z:members={n:z.read(n) for n in z.namelist()}
    ns='{http://schemas.openxmlformats.org/spreadsheetml/2006/main}';tree=ET.fromstring(members['xl/worksheets/sheet1.xml'])
    for cell in tree.iter(ns+'c'):
        if cell.attrib.get('r')=='C2':cell.find(ns+'v').text='5'
    members['xl/worksheets/sheet1.xml']=ET.tostring(tree,encoding='utf-8')
    with zipfile.ZipFile(path,'w',zipfile.ZIP_DEFLATED) as z:
        for name,data in members.items():z.writestr(name,data)
    value=converter(path,view='tables');assert value['status']=='completed',value
    sheets=value['value']['sheets'];assert sheets[0]['missing_formula_cache']==1
    rows=sheets[0]['rows'];assert rows[0][2]==5 or rows[0][2]=='5'
