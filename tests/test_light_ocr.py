import json
from pathlib import Path
from types import SimpleNamespace

import pytest
from PIL import Image
from backend import converter, ocr_parser
from backend.windows_ocr import WindowsOCRParser


def test_default_ocr_uses_windows_without_hidden_model(monkeypatch):
    monkeypatch.delenv('RAG_OCR_BACKEND', raising=False)
    assert isinstance(ocr_parser.make_ocr_parser(), WindowsOCRParser)


@pytest.mark.parametrize('failure', [False, True])
def test_windows_ocr_temporary_files_are_owned_and_failures_are_not_text(tmp_path, monkeypatch, failure):
    import backend.windows_ocr as windows
    monkeypatch.setenv('LES_WINDOWS_STATE_ROOT', str(tmp_path))
    def run(command, **kwargs):
        path = Path(command[-1])
        assert path.is_relative_to(tmp_path / 'storage/ocr-tmp') and path.is_file()
        assert kwargs['timeout'] == 120
        return SimpleNamespace(returncode=1 if failure else 0, stdout=json.dumps({'text': 'Планируем — 2033. 日本語'}).encode())
    monkeypatch.setattr(windows.subprocess, 'run', run)
    with Image.new('RGB', (100, 100), 'white') as image:
        if failure:
            with pytest.raises(RuntimeError, match='распознать'): WindowsOCRParser().ocr_page(image)
        else:
            assert WindowsOCRParser().ocr_page(image) == 'Планируем — 2033. 日本語'
    assert list((tmp_path / 'storage/ocr-tmp').iterdir()) == []


def test_failed_scan_never_becomes_searchable_warning(tmp_path, monkeypatch):
    from tests.pdf_fixtures import write_pdf
    image = tmp_path / 'page.png'
    Image.new('RGB', (50, 50), 'black').save(image)
    path = tmp_path / 'Скан с пробелом.pdf'
    write_pdf(path, [None])
    assert converter._parse_pdf_fast_text_layer(path) is None
    def fail(): raise RuntimeError('OCR unavailable')
    monkeypatch.setattr(ocr_parser, 'make_ocr_parser', fail)
    assert converter.convert_to_markdown(path) is None


def test_scan_after_three_text_pages_requires_ocr(tmp_path, monkeypatch):
    from tests.pdf_fixtures import write_pdf
    image = tmp_path / 'scan.png'
    Image.new('RGB', (50, 50), 'black').save(image)
    path = tmp_path / 'mixed.pdf'
    write_pdf(path, ['A long text page before a scanned page.'] * 3 + [None])
    called = []
    def recognize(source, **kwargs):
        called.append(source)
        return '## Стр. 4\n\nСодержимое последней сканированной страницы.'
    monkeypatch.setattr(ocr_parser, 'make_ocr_parser', lambda: SimpleNamespace(parse_pdf=recognize))
    result = converter._parse_pdf(path)
    assert called == [path]
    assert 'последней сканированной' in result
