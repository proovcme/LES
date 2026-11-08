"""Offline OCR using installed Windows languages; no hidden model or download."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile

from backend.runtime_paths import mutable_path


def _native(path: Path, dpi: int, *, kind: str, engine='windows', helper=None, **options) -> str | None:
    from backend.pdf_reader import _native_worker_result

    if (engine == 'windows' and os.name != 'nt') or not os.getenv('LES_NATIVE_PDF_WORKER') or not os.getenv('LES_NATIVE_PDFIUM'):
        return None
    helper = helper or (Path(__file__).resolve().parents[1] / 'tools/light_windows_ocr.ps1')
    scratch = mutable_path('storage/ocr-tmp').resolve()
    scratch.mkdir(parents=True, exist_ok=True)
    # Leave time for the supervising document worker to report a useful error.
    try:
        timeout = min(1_500_000, max(100, int(float(os.getenv('RAG_PARSE_FILE_TIMEOUT_SEC', '1800')) * 850)))
    except (OverflowError, ValueError):
        timeout = 1_500_000
    result = _native_worker_result(path, timeout_ms=timeout, kind=kind,
        ocr={'dpi': max(150 if engine == 'windows' else 1, min(2400, dpi)), 'engine': engine,
             'helper': str(helper), 'scratch': str(scratch), **options})
    if result is None:
        return None
    if result.get('status') == 'completed':
        value = result.get('value') or {}
        return value.get('text') if value.get('ocr') is True else None
    code = result.get('code')
    if code in {'PDF_SOURCE_LIMIT', 'PDF_TEXT_LIMIT', 'PDF_PAGE_LIMIT', 'OCR_UNAVAILABLE'}:
        return None
    raise RuntimeError(f'Не удалось распознать скан ({code or "OCR_READ_FAILED"}).')


class WindowsOCRParser:
    def ocr_image_file(self, path: Path) -> str | None:
        # WIC reads the first frame of these formats, as the former Pillow path does.
        if path.suffix.lower() in {'.jpg', '.jpeg', '.png', '.tif', '.tiff', '.bmp'}:
            try:
                return _native(path, 150, kind='image_ocr')
            except RuntimeError:
                # An installed WIC codec can reject images that Pillow can decode.
                return None
        return None

    def ocr_page(self, image) -> str:
        if os.name != 'nt':
            raise RuntimeError('Распознавание Windows доступно только в Windows.')
        helper = Path(__file__).resolve().parents[1] / 'tools/light_windows_ocr.ps1'
        temporary = mutable_path('storage/ocr-tmp').resolve()
        temporary.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=temporary) as directory:
            path = Path(directory) / 'page.png'
            bitmap = image.convert('RGB')
            try:
                bitmap.thumbnail((2400, 2400))
                bitmap.save(path)
            finally:
                bitmap.close()
            result = subprocess.run([
                str(Path(os.environ['SystemRoot']) / 'System32/WindowsPowerShell/v1.0/powershell.exe'),
                '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', str(helper),
                '-ImagePath', str(path),
            ], capture_output=True, timeout=120, creationflags=subprocess.CREATE_NO_WINDOW)
        if result.returncode:
            raise RuntimeError('Не удалось распознать скан. Проверьте наличие языка распознавания в настройках Windows.')
        payload = json.loads(result.stdout.decode('utf-8-sig', errors='strict'))
        return str(payload.get('text') or '').strip()

    def parse_pdf(self, pdf_path: Path, prompt=None, dpi: int = 150) -> str:
        text = _native(pdf_path, dpi, kind='pdf_ocr')
        if text is not None:
            return text
        from backend.pdf_reader import render_page, text_pages
        pages = []
        for number, text, scan in text_pages(pdf_path):
            if scan:
                image, _, _ = render_page(pdf_path, number, dpi=dpi)
                try:
                    if image.getextrema() == ((255,255), (255,255), (255,255)):
                        # A rendered, completely white sheet contains no visible
                        # text. Do not turn a real blank separator into OCR failure.
                        continue
                    text = self.ocr_page(image)
                finally:
                    image.close()
                if not text:
                    raise RuntimeError(f'На странице {number} не удалось распознать текст. Документ не считается готовым.')
            if text:
                pages.append(f'## Стр. {number}\n\n{text}')
        return '\n\n'.join(pages)
