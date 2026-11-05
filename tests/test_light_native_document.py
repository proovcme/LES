"""Real Windows structured storage, Unicode DOC pieces and citation contracts."""
import os
import struct
from pathlib import Path
import pytest
from backend import native_core


def _doc_fixture(path, *, encrypted=False, broken=False):
    # Minimal CFB v3: directory, two regular streams and FAT. No Office needed.
    header=bytearray(512)
    header[:8]=bytes.fromhex('d0cf11e0a1b11ae1')
    struct.pack_into('<HHHHH',header,24,0x3e,3,0xfffe,9,6)
    struct.pack_into('<I',header,44,1)
    struct.pack_into('<I',header,48,0)
    struct.pack_into('<I',header,56,4096)
    for at in (60,68):struct.pack_into('<I',header,at,0xfffffffe)
    for at in range(76,512,4):struct.pack_into('<I',header,at,17 if at==76 else 0xffffffff)
    directory=bytearray(512)
    for i,(name,kind,start,length,left,right,child) in enumerate([
        ('Root Entry',5,0xfffffffe,0,0xffffffff,0xffffffff,1),
        ('WordDocument',2,1,4096,2,0xffffffff,0xffffffff),
        ('1Table',2,9,4096,0xffffffff,0xffffffff,0xffffffff),
    ]):
        at=i*128;encoded=(name+'\0').encode('utf-16-le')
        directory[at:at+len(encoded)]=encoded
        struct.pack_into('<HBBIII',directory,at+64,len(encoded),kind,1,left,right,child)
        struct.pack_into('<IQ',directory,at+116,start,length)
    word=bytearray(4096);table=bytearray(4096)
    struct.pack_into('<HH',word,0,0xa5ec,0xc1)
    struct.pack_into('<H',word,10,0x200 | (0x100 if encrypted else 0))
    struct.pack_into('<H',word,32,14);struct.pack_into('<H',word,62,22)
    struct.pack_into('<H',word,152,93)
    unicode='Русский 日本語 🌲\r\x13HYPERLINK secret\x14Результат\x15\x07'
    data=unicode.encode('utf-16-le');word[2048:2048+len(data)]=data
    n=len(data)//2
    # One UTF-16 piece followed by compressed punctuation and text.
    ascii_text=b'Latin \x96 123\r';word[3072:3072+len(ascii_text)]=ascii_text
    p=struct.pack('<III',0,n,n+len(ascii_text))+struct.pack('<HIH',0,2048,0)+struct.pack('<HIH',0,0x40000000|6144,0)
    clx=b'\x02'+struct.pack('<I',len(p))+p
    table[:len(clx)]=clx
    struct.pack_into('<II',word,154+33*8,0,len(clx) if not broken else 4097)
    fat=[0xfffffffe]+list(range(2,9))+[0xfffffffe]+list(range(10,17))+[0xfffffffe,0xfffffffd]
    fat+= [0xffffffff]*(128-len(fat))
    path.write_bytes(header+directory+word+table+struct.pack('<128I',*fat))


@pytest.mark.skipif(os.name!='nt' or not os.getenv('LES_CORE_LIBRARY'),reason='Windows native DOC asset required')
@pytest.mark.parametrize('kind',['unicode','encrypted','truncated'])
def test_native_doc_reads_compound_streams_without_word(tmp_path,kind):
    path=tmp_path/'С пробелом 日本語.doc'
    _doc_fixture(path,encrypted=kind=='encrypted',broken=kind=='truncated')
    before=path.read_bytes()
    if kind=='unicode':
        result=native_core.call('document',request={'action':'legacy_doc','path':str(path)})
        assert result['text']=='Русский 日本語 🌲\nРезультат\nLatin – 123'
        assert result['extractor']=='les.ms-doc.v1'
    else:
        with pytest.raises(native_core.NativeCoreError,match='DOC_ENCRYPTED' if kind=='encrypted' else 'DOC_TRUNCATED'):
            native_core.call('document',request={'action':'legacy_doc','path':str(path)})
    assert path.read_bytes()==before


@pytest.mark.skipif(os.name!='nt' or not os.getenv('LES_CORE_LIBRARY'),reason='Windows native DOC asset required')
def test_doc_preview_uses_native_reader_and_declares_text_projection(tmp_path):
    from proxy.services.file_viewer_service import file_viewer_html,is_viewable_file
    path=tmp_path/'Документ.doc';_doc_fixture(path)
    before=path.read_bytes()
    assert is_viewable_file(path)
    html=file_viewer_html(path,path_id='source',document_id='doc-id')
    assert 'Русский 日本語 🌲' in html and 'Latin – 123' in html
    assert 'Текстовый предпросмотр DOC' in html and 'HYPERLINK secret' not in html
    assert path.read_bytes()==before


@pytest.mark.skipif(not os.getenv('LES_CORE_LIBRARY'),reason='Native asset required')
def test_citation_normalization_does_not_invent_evidence():
    from proxy.services.source_locator_service import normalize_answer_citations,citation_indexes,evidence_counts
    sources=[{'index':1,'header':'[Источник 1 | чертёж 87.pdf | TABLE]'}, {'index':2}]
    value=normalize_answer_citations('Факт *Источник 1 | чертёж 87.pdf | TABLE]*. [Источники 1–2] [999] `пример [Источник 2]`',sources)
    assert value['indexes']==[1,2] and value['unknown_indexes']==[999]
    assert value['header_repairs']==1 and 'чертёж' not in value['answer']
    assert '`пример [Источник 2]`' in value['answer']
    assert citation_indexes('[Источник 1 | 87.pdf] [2] `пример [Источник 3]`')==[1,2]
    assert evidence_counts(answer=value['answer'],source_map=sources,found_count=10)=={'found':10,'model_visible':2,'cited':2}
    absent=normalize_answer_citations('Источник 1; количество источников 2. [999]',sources)
    assert absent['indexes']==[] and absent['unknown_indexes']==[999]


@pytest.mark.skipif(not os.getenv('LES_CORE_LIBRARY'),reason='Native asset required')
def test_tool_evidence_handles_use_the_same_parser_and_exclude_code():
    from proxy.services.source_locator_service import normalize_answer_citations,evidence_counts
    sources=[{'index':1,'evidence_ref':'Q1.H1'},{'index':2,'evidence_ref':'Q1.H2'}]
    text='Факт Q1.H1. ``Q1.H2 [Источник 2]``\n~~~text\nQ1.H2 [2]\n~~~\n[Источник 2](https://example.org)'
    result=normalize_answer_citations(text,sources)
    assert result['indexes']==[1]
    assert 'Факт [Источник 1].' in result['answer']
    assert evidence_counts(answer=text,source_map=sources,found_count=5)['cited']==1


def test_temporary_office_sources_are_retained_but_never_retried(tmp_path):
    import sqlite3
    from backend.document_catalog import MetaDB
    path=tmp_path/'catalog.db'
    db=MetaDB(str(path))
    with sqlite3.connect(path) as conn:
        conn.executemany("INSERT INTO documents(id,dataset_id,file_name,status,chunk_count) VALUES (?,?,?,?,?)",[
            ('owner','scope','folder/~$Документ.doc','ERROR',0),
            ('real','scope','folder/Документ.doc','PENDING',0),
            ('old-points','scope','~$retained.doc','INDEXED',1),
        ])
    db=MetaDB(str(path))
    assert db.get_pending_files('scope')==['folder/Документ.doc']
    with sqlite3.connect(path) as conn:
        assert conn.execute("SELECT status,error_code FROM documents WHERE id='owner'").fetchone()==('SKIPPED','TEMPORARY_SOURCE')
        assert conn.execute("SELECT status FROM documents WHERE id='old-points'").fetchone()==('INDEXED',)
        assert conn.execute('SELECT count(*) FROM documents').fetchone()[0]==3


def test_pdf_geometry_limit_keeps_text_and_never_runs_intersections(tmp_path,monkeypatch):
    from tests.pdf_fixtures import write_pdf
    from backend.pdf_reader import extract_markdown
    from types import SimpleNamespace
    import pdfplumber
    path=write_pdf(tmp_path/'drawing.pdf',['Readable engineering evidence'])
    closed=[]
    class Page:
        edges=[None]*2001
        def close(self):closed.append(True)
        def extract_tables(self):raise AssertionError('Unbounded geometry must not run')
    class Document:
        pages=[Page()]
        def __enter__(self):return self
        def __exit__(self,*args):pass
    monkeypatch.setattr(pdfplumber,'open',lambda *args,**kwargs:Document())
    text,scan=extract_markdown(path)
    assert 'Readable engineering evidence' in text and not scan
    assert closed==[True]


def test_pdf_passport_releases_layout_even_on_failure(tmp_path,monkeypatch):
    from tests.pdf_fixtures import write_pdf
    from proxy.services import pdf_contour_service as contour
    import pdfplumber
    path=write_pdf(tmp_path/'source.pdf',['Source evidence'])
    closed=[]
    class Page:
        def close(self):closed.append(True)
    class Document:
        pages=[Page()]
        def __enter__(self):return self
        def __exit__(self,*args):pass
    monkeypatch.setattr(pdfplumber,'open',lambda *args,**kwargs:Document())
    def fail(*args,**kwargs):raise RuntimeError('layout failure')
    monkeypatch.setattr(contour,'_page_passport',fail)
    with pytest.raises(RuntimeError,match='layout failure'):contour.rag_page_metadata(path,[1])
    assert closed==[True]


def test_pdf_native_geometry_guard_runs_before_pdfminer(tmp_path,monkeypatch):
    from tests.pdf_fixtures import write_pdf
    from backend.pdf_reader import complex_geometry_pages,extract_markdown
    from proxy.services import pdf_contour_service as contour
    path=write_pdf(tmp_path/'complex.pdf',['Readable source text on a complex drawing'],drawing_segments=2001)
    before=path.read_bytes()
    assert complex_geometry_pages(path)=={1}
    def fail(*args,**kwargs):raise AssertionError('Detailed geometry must be deferred')
    monkeypatch.setattr(contour,'_page_passport',fail)
    assert contour.rag_page_metadata(path,[1])=={}
    assert 'Readable source text' in extract_markdown(path)[0]
    assert path.read_bytes()==before


def test_fast_pdf_text_is_not_disabled_by_external_worker_isolation(tmp_path,monkeypatch):
    from backend import converter
    from tests.pdf_fixtures import write_pdf
    path=write_pdf(tmp_path/'plain.pdf',['Text extraction must precede isolation selection'])
    monkeypatch.setenv('RAG_CONVERT_SUBPROCESS_ENABLED','false')
    def fail(*args,**kwargs):raise AssertionError('Must use fast text, not full table conversion')
    monkeypatch.setattr(converter,'convert_to_markdown',fail)
    assert 'Text extraction' in converter.convert_to_markdown_for_indexing(path)


def test_empty_text_operators_and_short_vector_text_are_not_failed_scans(tmp_path):
    from tests.pdf_fixtures import write_pdf
    from backend.pdf_reader import text_pages
    blank=write_pdf(tmp_path/'blank.pdf',[''])
    short=write_pdf(tmp_path/'separator.pdf',['02'],drawing_segments=10)
    scan=write_pdf(tmp_path/'raster.pdf',[None])
    assert list(text_pages(blank))==[(1,'',False)]
    assert list(text_pages(short))==[(1,'02',False)]
    assert list(text_pages(scan))==[(1,'',True)]


def test_proven_white_raster_separator_is_blank_without_fake_text(tmp_path,monkeypatch):
    from PIL import Image
    from backend.windows_ocr import WindowsOCRParser
    path=tmp_path/'white.pdf'
    with Image.new('RGB',(200,300),'white') as image:image.save(path,'PDF')
    parser=WindowsOCRParser()
    def fail(*args,**kwargs):raise AssertionError('A uniformly white rendered page needs no OCR')
    monkeypatch.setattr(parser,'ocr_page',fail)
    assert parser.parse_pdf(path)==''


@pytest.mark.skipif(os.name!='nt' or not os.getenv('LES_CORE_LIBRARY'),reason='Windows native DOC asset required')
def test_worker_keeps_encryption_failure_specific_and_logs_cause(tmp_path,monkeypatch,caplog):
    from backend.format_process import prepare,stop,FormatProcessError
    from backend.qdrant_support import StructureAwareSplitter
    monkeypatch.setenv('LES_WINDOWS_STATE_ROOT',str(tmp_path/'state'))
    path=tmp_path/'encrypted.doc';_doc_fixture(path,encrypted=True)
    try:
        with pytest.raises(FormatProcessError,match='паролем') as error:
            prepare(path,tmp_path/'data',path.name,'test',StructureAwareSplitter(chunk_size=1000,chunk_overlap=100),{},True)
        assert error.value.code=='DOC_ENCRYPTED'
        assert 'DOC_ENCRYPTED' in caplog.text and 'diagnostics=Traceback' in caplog.text
    finally:stop()


@pytest.mark.skipif(not os.getenv('LES_CORE_LIBRARY'),reason='Native format host required')
def test_isolated_interpreter_receives_explicit_runtime_path(tmp_path,monkeypatch):
    from backend import format_process
    from backend.qdrant_support import StructureAwareSplitter
    monkeypatch.setenv('LES_WINDOWS_STATE_ROOT',str(tmp_path/'state'))
    path=tmp_path/'Изолированный путь 日本語.txt'
    path.write_text('Проверяем документ: UTF-8 и 日本語, точный контрольный код FOREST-783.',encoding='utf-8')
    original=native_core.call
    def isolated(operation,**kwargs):
        if operation=='worker_run':
            request=dict(kwargs['request'])
            request['args']=['-I',*request['args']]
            kwargs['request']=request
        return original(operation,**kwargs)
    monkeypatch.setattr(native_core,'call',isolated)
    try:
        _,nodes=format_process.prepare(path,tmp_path/'data',path.name,'test',StructureAwareSplitter(chunk_size=1000,chunk_overlap=100),{},True)
        assert any('FOREST-783' in n['text'] and '日本語' in n['text'] for n in nodes)
    finally:format_process.stop()

def test_integrity_pdf_inventory_avoids_layout_and_keeps_page_numbers(tmp_path, monkeypatch):
    import sys
    from tests.pdf_fixtures import write_pdf
    from backend.qdrant_integrity import QdrantIntegrity
    path = write_pdf(tmp_path / 'dense.pdf', ['Evidence ' * 30, '', 'Original ' * 30], drawing_segments=2500)
    monkeypatch.setitem(sys.modules, 'pdfplumber', None)
    assert QdrantIntegrity._expected_pdf_text_pages(path) == {1, 3}


def test_integrity_pdf_read_failure_is_not_an_empty_success(tmp_path):
    from backend.qdrant_integrity import QdrantIntegrity
    from backend.extraction_errors import DocumentExtractionError
    path = tmp_path / 'broken.pdf'
    path.write_bytes(b'not a PDF')
    with pytest.raises(DocumentExtractionError) as error:
        QdrantIntegrity._expected_pdf_text_pages(path)
    assert error.value.code == 'FORMAT_READ_FAILED'

@pytest.mark.parametrize('indexed,expected,state', [({1,2},{1},'healthy'), ({1,2},{1,3},'repairable'), ({1},{1},'healthy')])
def test_integrity_accepts_additional_ocr_pages_but_detects_missing_text(tmp_path, monkeypatch, indexed, expected, state):
    from types import SimpleNamespace
    from backend.qdrant_integrity import QdrantIntegrity
    from backend import qdrant_support as support
    path = tmp_path / 'source.pdf'
    path.write_bytes(b'fixture source; page reader supplied separately')
    ids = {'point-1','point-2'}
    db = SimpleNamespace(dataset_integrity_rows=lambda dataset: [{'file_name':'source.pdf', 'source_path':str(path), 'status':'INDEXED', 'chunk_count':2}],
        lexical_integrity_projection=lambda dataset: {'files':{'source.pdf':ids}, 'fts_available':True, 'lexical_ids':ids, 'fts_ids':ids})
    adapter = SimpleNamespace(db=db, qdrant_url='http://localhost:6333', content_dir=tmp_path,
        _sync_dataset_point_projection=lambda client,dataset: {'source.pdf':{'ids':ids,'pages':indexed}},
        _sync_count_file_vector_points=lambda *args:2, _expected_pdf_text_pages=lambda source:expected)
    monkeypatch.setattr(support.qdrant_client,'QdrantClient',lambda **kwargs:object())
    monkeypatch.setattr(support,'qdrant_client_options',lambda url:{})
    monkeypatch.setattr(support,'index_contract_status',lambda:{'compatible':True})
    result = QdrantIntegrity._audit_dataset_integrity(adapter,'dataset')
    assert result['state'] == state
    assert result['damaged_files'] == (1 if state=='repairable' else 0)
    assert result['file_checks'][0]['indexed_text_pages'] == len(indexed)
