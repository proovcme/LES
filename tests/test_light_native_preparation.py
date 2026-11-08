"""Index preparation preserves source text and physical page positions."""
import os
import json
import sqlite3
import struct
from pathlib import Path
from uuid import UUID
from zipfile import ZIP_DEFLATED, ZipFile

import pytest

from backend import native_core
from backend.converter import _parse_docx_native
from backend.document_catalog import MetaDB
from backend.inference.bm25_sparse import _encode_python, encode_bm25
from backend.qdrant_ingestion import QdrantIngestion
from backend.qdrant_nodes import QdrantNodes
from backend.structure_splitter import StructureAwareSplitter
from backend.windows_ocr import _native
from tests.pdf_fixtures import write_pdf


@pytest.mark.parametrize("tokenizer", [None, lambda text: len(text.split())])
def test_native_final_chunks_preserve_text_identifiers_and_quality(monkeypatch, tokenizer):
    if not native_core.supports("embedding_chunks"):
        pytest.skip("Build the native embedding chunk finalizer")
    nodes = [{
        "text": f"Сначала проект 東京 🌲\x01   проверяется. {'A' * 256}==; "
                f"Потом вложение DATA:image/png;base64,{'B' * 144}!\n\n\n\n"
                f"Не payload: {'C' * 259}===\u00a0{('раздел с деталями ' * 28).strip()}",
        "doc_id": "редакция-7",
        "payload": {"page": 2, "source_ref": "Книга.pdf#page=2", "parent_node_id": "section-1"},
    }]
    chunking = {"unit": "chars" if tokenizer is None else "tokens",
                "chunk_size": 80 if tokenizer is None else 9, "len_fn": tokenizer}
    monkeypatch.setenv("LES_CORE_MODE", "python")
    previous = QdrantNodes._finalize_embedding_nodes(nodes, chunking=chunking)
    monkeypatch.setenv("LES_CORE_MODE", "native")
    assert QdrantNodes._finalize_embedding_nodes(nodes, chunking=chunking) == previous
    assert len(previous) > 2
    assert previous[0]["payload"]["content_quality"] == {
        "data_uri_removed": 1, "base64_runs_removed": 1,
        "control_chars_removed": 1, "sanitized": True,
    }


def test_native_manual_batch_resolves_registered_sources_and_never_guesses(tmp_path):
    if not native_core.supports("index_sources"):
        pytest.skip("Build the native index source planner")
    root = tmp_path / "Набор 東京"
    database = tmp_path / "meta.db"
    MetaDB(str(database))
    names = [
        "Раздел/Кабель 1.txt", "Раздел/Старый файл.txt", "Подраздел/Кабель 1.txt",
        "Раздел/Дубль.txt", "Подраздел/Дубль.txt", "_parquet/stage.txt", "~$замок.docx",
    ]
    for name in names:
        file = root / name
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text("Исходный текст", encoding="utf-8")
    external = tmp_path / "Скан снаружи.pdf"
    external.write_bytes(b"fixture")
    nested = str(Path(names[0]))  # On Windows this is the old backslash spelling.
    records = [
        (nested, "", 3, "text"), ("Старый файл.txt", "", 2, "text"),
        ("Дубль.txt", "", 4, "text"), ("Пропал.txt", "", 5, "text"),
        ("../вне.txt", "", 6, "text"), (external.name, str(external), 1, "needs_ocr"),
        ("~$замок.docx", "", 1, "text"),
    ]
    with sqlite3.connect(database) as conn:
        conn.execute("INSERT INTO datasets(id,name) VALUES('test','Набор'),('other','Другой')")
        for i, (name, path, size, complexity) in enumerate(records):
            conn.execute("INSERT INTO documents(id,dataset_id,file_name,status,source_path,file_size,complexity)"
                         " VALUES(?, 'test', ?, 'PENDING', ?, ?, ?)", (str(i), name, path, size, complexity))
        conn.execute("INSERT INTO documents(id,dataset_id,file_name,status)"
                     " VALUES('foreign','other','other.txt','PENDING')")

    request = {"database": str(database), "root": str(root), "dataset_id": "test"}
    plan = native_core.required("index_sources", request={**request, "limit": 2})
    assert (plan["total_files"], plan["matched_internal"]) == (5, 2)
    assert [(row["db_key"], row["file_key"], row["error"]) for row in plan["files"]] == [
        ("Старый файл.txt", names[1], None), (nested, names[0], None),
    ]
    plan = native_core.required("index_sources", request=request)
    assert [(row["db_key"], row["error"]) for row in plan["files"][2:]] == [
        ("Дубль.txt", "INDEX_SOURCE_AMBIGUOUS"), ("Пропал.txt", "INDEX_SOURCE_MISSING"),
        ("../вне.txt", "INDEX_SOURCE_PLAN_INVALID"), (external.name, None),
    ]
    assert Path(plan["files"][-1]["path"]) == external
    assert native_core.required("index_sources", request={**request, "count_only": True})["remaining_pending"] == 6
    fingerprint = QdrantIngestion._source_fingerprint(root / names[0])
    assert fingerprint["file_mtime"] == pytest.approx((root / names[0]).stat().st_mtime, rel=0, abs=0.000001)
    assert fingerprint["file_size"] == len("Исходный текст".encode())


def test_legacy_sparse_native_matches_persisted_python_term_ids_and_frequencies():
    import random
    import re
    import sys

    if not native_core.supports('legacy_sparse'):
        pytest.skip('Build the native compatibility sparse encoder')
    vocabulary = re.compile(r'[a-z0-9а-яёı]')
    casefold_exceptions = [
        chr(point) for point in range(sys.maxunicode + 1)
        if (character := chr(point)) != (folded := character.casefold())
        and vocabulary.search(folded) and not re.fullmatch('[A-ZА-ЯЁ]', character)
    ]
    samples = [
        '', '... — ——', 'PE РЕ N BB_63', '№ 3-4; КЛ-4.13130, №112',
        'ёлка ЁЛКИ ЕЛКА ГОСТ сп ПУЭ', 'проложить кабель, кабеля и кабелем',
        'необходимо выполнять что нужно где нормы', '東京 章🌲', 'ſ K İ ı ß ẞ',
    ]
    samples.extend(f'ab{character}cd 9.4 ab{character}cd' for character in casefold_exceptions)
    alphabet = 'PE-N.кл_ЁİıſK5 /ёїßﬃﬆ：Ⅰѡ東京😀ᲄ'
    randomizer = random.Random(811)
    samples.extend(''.join(randomizer.choices(alphabet, k=size)) for size in range(256))
    for sample in samples:
        assert list(encode_bm25(sample).items()) == list(_encode_python(sample).items()), repr(sample)


def test_native_point_spool_merges_vector_batches_and_keeps_document_locators(tmp_path):
    if not native_core.supports('index_points'):
        pytest.skip('Build the native point assembler')
    spool = tmp_path / 'Точки проекта 東京.jsonl'
    cache = tmp_path / 'Векторы 東京.cache'
    texts = ['Раздел проекта', 'Таблица, строка 7', 'Следующий раздел']
    nodes = [{'text': text, 'doc_id': 'doc-7', 'payload': {
        'content_hash': f'{i:040x}', 'source_page': 9, 'row': i + 7, 'parent_id': 'section',
    }} for i, text in enumerate(texts)]
    empty = {'text': '... —', 'payload': {'content_hash': 'empty'}}
    all_nodes = [*nodes[:2], empty, nodes[2]]
    request = {'spool': str(spool), 'count': len(all_nodes), 'batch_size': 2, 'dimension': 2,
        'vector_names': {'dense': 'meaning', 'sparse': 'terms'}, 'dataset_id': 'ds',
        'file_name': 'Паспорт 東京.pdf', 'source_sha256': 'a' * 64,
        'embedding': {'model_id': 'fixture:embedding'}, 'embedding_fingerprint': 'bound',
        'cache': str(cache)}
    cache.write_bytes(b'LESCACH1' + struct.pack('<I', 2) + b'0' * 40 + struct.pack('<2f', 1, 0))
    calls = []

    def embed(batch):
        calls.append(batch)
        return [[0, 1]] * len(batch)

    result = native_core.prepare_index_points(request, all_nodes, embed)
    points = [json.loads(line) for line in spool.read_text(encoding='utf-8').splitlines()]
    assert calls == [[texts[1]], [texts[2]]]
    assert (result['cache_hits'], result['embedded']) == (1, 2)
    assert result['skipped_sparse'] == 1
    assert result['cache_read_failed'] is False
    assert result['ids'] == [row['id'] for row in points]
    assert len(set(result['ids'])) == 3 and all(UUID(key).version == 4 for key in result['ids'])
    assert [row['vector']['meaning'] for row in points] == [[1, 0], [0, 1], [0, 1]]
    for index, point in enumerate(points):
        assert dict(zip(point['vector']['terms']['indices'], point['vector']['terms']['values'])) == _encode_python(texts[index])
        assert point['payload'] == {
            **nodes[index]['payload'], 'dataset_id': 'ds', 'file_name': request['file_name'],
            'source_sha256': 'a' * 64, 'text': texts[index], 'doc_id': 'doc-7',
            'embedding_fingerprint': 'bound', 'embedding_model_id': 'fixture:embedding',
            **dict.fromkeys(('embedding_backend', 'embedding_profile', 'embedding_coreml_model',
                'embedding_coreml_seq_len', 'embedding_coreml_compute_units', 'embedding_coreml_fallback'), ''),
        }
    with pytest.raises(native_core.NativeCoreError, match='INDEX_SPOOL_WRITE_FAILED'):
        native_core.prepare_index_points(request, all_nodes, embed)
    assert spool.is_file()

    for index, (bad_result, code) in enumerate([
        ([], 'EMBEDDING_COUNT_MISMATCH'), ([[False, 0]], 'EMBEDDING_VECTOR_INVALID'),
        ([[1e100, 0]], 'EMBEDDING_VECTOR_INVALID'), ([[float('nan'), 0]], 'EMBEDDING_VECTOR_INVALID'),
    ]):
        partial = tmp_path / f'partial-{index}.jsonl'
        with pytest.raises(native_core.NativeCoreError, match=code):
            native_core.prepare_index_points({**request, 'spool': str(partial)}, all_nodes,
                lambda batch: [[0, 1]] if batch == [texts[1]] else bad_result)
        assert not partial.exists()
    empty_spool = tmp_path / 'empty.jsonl'
    with pytest.raises(native_core.NativeCoreError, match='NO_SEARCHABLE_TEXT'):
        native_core.prepare_index_points({**request, 'spool': str(empty_spool), 'count': 1},
            [empty], lambda batch: pytest.fail(f'Embedded non-searchable text: {batch}'))
    assert not empty_spool.exists()
    cache.write_bytes(cache.read_bytes()[:-1])
    result = native_core.prepare_index_points({**request, 'spool': str(tmp_path / 'fallback.jsonl')}, all_nodes, embed)
    assert result['cache_read_failed'] and result['cache_hits'] == 0 and result['embedded'] == 3


@pytest.mark.parametrize('tokenizer', [None, lambda value: max(1, len(value) // 3)])
def test_native_split_matches_the_previous_chunk_boundaries(tokenizer):
    if not native_core.supports('document_chunks'):
        pytest.skip('Build the Rust document core')
    clauses = '\n\n'.join(f'5.{i} Условие 東京. ' + 'Сохраняем кириллицу и 🌲. ' * i for i in range(1, 9))
    text = 'Введение.\n\n# Раздел\n\n' + clauses + '\n\n§ 8.3\n' + '文' * 101
    splitter = StructureAwareSplitter(chunk_size=36, chunk_overlap=6, len_fn=tokenizer)
    assert splitter._split(text) == splitter._split_compatibility(text)


def test_markdown_headings_inside_code_stay_with_their_section():
    from llama_index.core.node_parser import MarkdownNodeParser
    from llama_index.core.schema import Document

    if not native_core.supports('document_chunks'):
        pytest.skip('Build the Rust document core')
    text = 'Введение\n##\tВторой раздел\n```sh\n# Комментарий\n```\n### 東京\nТекст.'
    expected = [node.text for node in MarkdownNodeParser().get_nodes_from_documents([Document(text=text)])]
    result = native_core.required('document', request={'action': 'markdown_sections', 'text': text})
    assert result['chunks'] == expected


def test_native_tokenizer_is_pinned_and_used_by_the_splitter(tmp_path, monkeypatch):
    from backend import rag_config

    if not native_core.supports('document_tokenizer'):
        pytest.skip('Build the native tokenizer')
    path = tmp_path / 'Токенизатор 東京.json'
    spec = {'version': '1.0', 'truncation': None, 'padding': None, 'added_tokens': [],
            'normalizer': None, 'pre_tokenizer': {'type': 'Whitespace'}, 'post_processor': None,
            'decoder': None, 'model': {'type': 'WordPiece', 'unk_token': '[UNK]',
            'continuing_subword_prefix': '##', 'max_input_chars_per_word': 100,
            'vocab': {'[UNK]': 0, 'кабель': 1, 'лес': 2, '東京': 3, '.': 4}}}
    path.write_text(json.dumps(spec, ensure_ascii=False), encoding='utf-8')
    monkeypatch.setenv('RAG_TOKENIZER_FILE', str(path))
    monkeypatch.setenv('RAG_CHUNK_UNIT', 'tokens')
    monkeypatch.setenv('RAG_INDEX_CONTRACT_PATH', str(tmp_path / 'contract.json'))
    counter = rag_config.token_length_fn()
    assert isinstance(counter, native_core.LocalTokenizer)
    assert counter('кабель лес 東京.') == 4
    assert rag_config.index_contract_payload()['chunk_tokenizer_sha256'] == counter.asset['sha256']
    text = 'кабель лес. 東京 лес. кабель кабель.'
    assert native_core.split_chunks(text, 5, 1, tokenizer=counter) == native_core.split_chunks(text, 5, 1, tokenizer=lambda part: counter(part))
    path.write_text(path.read_text(encoding='utf-8') + '\n', encoding='utf-8')
    with pytest.raises(native_core.NativeCoreError, match='DOCUMENT_TOKENIZER_CHANGED'):
        counter('лес')
    monkeypatch.delenv('RAG_TOKENIZER_FILE')
    monkeypatch.setattr(rag_config, 'read_index_contract', lambda: {'chunk_unit': 'chars'})
    assert rag_config.chunking_config()['unit'] == 'chars'


def test_docx_headings_runs_and_rectangular_tables_keep_text(tmp_path):
    if not native_core.supports('document_docx'):
        pytest.skip('Build the Rust document core')
    path = tmp_path / 'Пояснения 東京.docx'

    def make(extra=''):
        with ZipFile(path, 'w', compression=ZIP_DEFLATED) as archive:
            archive.writestr('word/document.xml', '''<w:document
              xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
              <w:body><w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr>
              <w:r><w:t>Раздел 5.2</w:t></w:r></w:p>
              <w:p><w:r><w:t xml:space="preserve">Кабель &amp; </w:t><w:t>東京 &#x1f332;</w:t></w:r>
              <w:r><w:br/><w:t>Текст после переноса.</w:t></w:r></w:p>''' + extra + '</w:body></w:document>')

    make()
    assert _parse_docx_native(path) == '## Раздел 5.2\n\nКабель & 東京 🌲\nТекст после переноса.'
    make('<w:tbl><w:tr><w:tc><w:p><w:r><w:t>В ячейке</w:t></w:r></w:p></w:tc>'
         '<w:tc><w:p/></w:tc></w:tr></w:tbl>')
    assert _parse_docx_native(path).endswith('|  |  |\n| --- | --- |\n| В ячейке |  |')
    make('<w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr>'
         '<w:p><w:r><w:t>строка</w:t></w:r></w:p></w:tc></w:tr></w:tbl>')
    assert _parse_docx_native(path) is None


def test_native_windows_pdf_ocr_renders_only_nonblank_scan_pages(tmp_path, monkeypatch):
    import pypdfium2_raw
    from backend import pdf_reader

    api = os.getenv('LES_TEST_NATIVE_API')
    asset = Path(pypdfium2_raw.__file__).with_name('pdfium.dll')
    if os.name != 'nt' or not api or not asset.is_file() or not native_core.supports('worker_pdf'):
        pytest.skip('Build the Windows PDF worker')
    helper = tmp_path / 'ocr-fixture.ps1'
    helper.write_text('''param([string]$ImagePath)
$bytes = [IO.File]::ReadAllBytes($ImagePath)
if ($bytes[0] -ne 66 -or $bytes[1] -ne 77 -or $bytes.Length -le 54) { exit 1 }
[Console]::Write('{"text":"PROOF 2047"}')
''', encoding='utf-8')
    path = write_pdf(tmp_path / 'Скан 東京.pdf', [
        'Existing text stays on its original page.', None, b'1 1 1 rg 0 0 300 300 re f', 'Last',
    ])
    original = path.read_bytes()
    monkeypatch.setenv('LES_NATIVE_PDF_WORKER', api)
    monkeypatch.setenv('LES_NATIVE_PDFIUM', str(asset))
    try:
        result = pdf_reader._native_worker_result(path, kind='pdf_ocr', timeout_ms=15000,
            ocr={'dpi': 150, 'helper': str(helper), 'scratch': str(tmp_path)})
        assert result['value']['ocr'] is True
        assert result['value']['text'] == ('## Стр. 1\n\nExisting text stays on its original page.\n\n'
                                            '## Стр. 2\n\nPROOF 2047\n\n## Стр. 4\n\nLast')
        assert not list(tmp_path.glob('*.bmp'))
        assert path.read_bytes() == original
        adapter = tmp_path / 'tesseract-fixture.cmd'
        adapter.write_text('@echo off\nif not exist "%~1" exit /b 1\n'
            'if not "%~4"=="rus+eng" exit /b 2\nif not "%~6"=="6" exit /b 3\necho TESSERACT 2048\n', encoding='ascii')
        result = pdf_reader._native_worker_result(path, kind='pdf_ocr', timeout_ms=15000,
            ocr={'engine': 'tesseract', 'language': 'rus+eng', 'psm': 6, 'dpi': 150,
                 'helper': str(adapter), 'scratch': str(tmp_path)})
        assert result['value']['text'].count('TESSERACT 2048') == 4
        assert not list(tmp_path.glob('*.bmp'))
    finally:
        native_core.required('worker_stop')
