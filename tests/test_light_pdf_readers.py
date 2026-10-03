from concurrent.futures import ThreadPoolExecutor
from io import BytesIO
import pytest
from PIL import Image
from backend.pdf_reader import render_page
from backend import converter
from backend.mail_profile import _extract_pdf_text_inprocess, _extract_pdf_text_subprocess
from proxy.services.pdf_contour_service import render_page_preview
from proxy.services.pdf_viewer_service import pdf_file_info
from tests.pdf_fixtures import write_pdf


def test_pdf_text_provenance_preview_and_mail(tmp_path):
    path = write_pdf(tmp_path / '日本語 и пробел #.pdf', ['Document evidence on page one', 'Second page evidence'])
    before = path.read_bytes()
    result = converter._parse_pdf(path)
    assert 'Стр. 1' in result and 'Стр. 2' in result and 'Second page evidence' in result
    assert pdf_file_info(path)['page_count'] == 2
    for extract in (_extract_pdf_text_inprocess, _extract_pdf_text_subprocess):
        text, error = extract(before)
        assert not error and 'Second page evidence' in text
    data = render_page_preview(path, page_number=2, highlight_bbox=(0, 0, 30, 30))
    with Image.open(BytesIO(data)) as image:
        assert image.width > 0 and image.getpixel((10, 10)) != (255, 255, 255)
    assert path.read_bytes() == before


def test_parallel_rendering_and_invalid_regions(tmp_path):
    path = write_pdf(tmp_path / 'page.pdf', ['Concurrent rendering'])
    def render(_):
        image, _, _ = render_page(path, 1, width=320)
        try: return image.size
        finally: image.close()
    with ThreadPoolExecutor(max_workers=4) as pool:
        assert list(pool.map(render, range(12))) == [(320, 320)] * 12
    for box in ((500, 500, 600, 600), (0, 0, float('nan'), 50)):
        with pytest.raises(ValueError): render_page(path, 1, bbox=box)
    with pytest.raises(ValueError): render_page(path, 2)


def test_disabled_ocr_does_not_index_partial_mixed_document(tmp_path, monkeypatch):
    path = write_pdf(tmp_path / 'mixed.pdf', ['A real text page with enough characters', None])
    monkeypatch.setenv('RAG_OCR_ENABLED', 'false')
    assert converter._parse_pdf(path) is None
