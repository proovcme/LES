"""Offline OCR using installed Windows languages; no hidden model or download."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile

from backend.runtime_paths import mutable_path


class WindowsOCRParser:
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
