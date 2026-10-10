"""Offline scan recognition using macOS Vision, without model downloads."""
from __future__ import annotations
import json
from pathlib import Path
import subprocess
import tempfile
import sys

from backend.runtime_paths import mutable_path
from backend.windows_ocr import WindowsOCRParser


class MacOSOCRParser(WindowsOCRParser):
    def ocr_page(self, image) -> str:
        if sys.platform != 'darwin':
            raise RuntimeError('Vision OCR requires macOS')
        root=Path(__file__).resolve().parents[1]
        binary=root/'native/vision/les-ocr'
        if not binary.is_file():
            raise RuntimeError('Vision OCR не установлен. Выполните установку macOS preview с Xcode Command Line Tools.')
        temporary=mutable_path('storage/ocr-tmp').resolve();temporary.mkdir(parents=True,exist_ok=True)
        with tempfile.TemporaryDirectory(dir=temporary) as directory:
            path=Path(directory)/'page.png'
            bitmap=image.convert('RGB')
            try:
                bitmap.thumbnail((3200,3200));bitmap.save(path)
            finally:
                bitmap.close()
            result=subprocess.run([str(binary),str(path)],capture_output=True,timeout=120)
        if result.returncode:
            raise RuntimeError('Не удалось распознать скан средствами macOS Vision.')
        return str(json.loads(result.stdout.decode('utf-8')).get('text') or '').strip()
