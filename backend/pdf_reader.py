"""PDF text and bounded rendering without process-global PDFium races."""
from __future__ import annotations

import math
from pathlib import Path
from threading import RLock

# PDFium is not thread safe, including calls on separate documents. Keep all
# native object lifetimes inside this lock; return an independent PIL image.
_PDFIUM_LOCK = RLock()


def render_page(path: str | Path, number: int, *, width: int = 1200, dpi: int | None = None,
                bbox=None):
    import pypdfium2 as pdfium

    with _PDFIUM_LOCK, pdfium.PdfDocument(str(path)) as document:
        if not 1 <= number <= len(document):
            raise ValueError('Страница PDF вне диапазона')
        page = document[number - 1]
        try:
            w, h = page.get_size()
            clip = clipped_box(bbox, w, h) if bbox is not None else (0, 0, w, h)
            scale = min(3.0, max(0.5, max(320, min(width, 1800)) / (clip[2] - clip[0])))
            if dpi is not None:
                scale = min(max(dpi, 150) / 72, 2400 / max(w, h))
            # Bound the complete bitmap even for enormous engineering sheets.
            scale = min(scale, 5400 / max(w, h))
            bitmap = page.render(scale=scale)
            try:
                image = bitmap.to_pil().convert('RGB')
            finally:
                bitmap.close()
            if bbox is not None:
                cropped = image.crop(tuple(round(v * scale) for v in clip))
                image.close()
                image = cropped
            return image, scale, (w, h)
        finally:
            page.close()


def clipped_box(box, width, height):
    if len(box) != 4 or not all(math.isfinite(float(v)) for v in box):
        raise ValueError('Некорректная область evidence')
    x0, y0, x1, y1 = map(float, box)
    result = (max(0, x0), max(0, y0), min(width, x1), min(height, y1))
    if result[2] - result[0] < 1 or result[3] - result[1] < 1:
        raise ValueError('Область evidence не пересекает страницу')
    return result


def extract_markdown(path: Path, *, tables: bool = True) -> tuple[str, bool]:
    """Keep page provenance and table rows; report scans instead of inventing text."""
    import pdfplumber

    parts = []
    needs_ocr = False
    with pdfplumber.open(path) as document:
        for number, page in enumerate(document.pages, 1):
            text = (page.extract_text() or '').strip()
            needs_ocr |= len(text) < 20 and bool(page.images)
            if tables:
                for rows in page.extract_tables() or []:
                    clean = [[str(cell or '').replace('\n', ' ').replace('|', '\\|') for cell in row] for row in rows]
                    if clean:
                        rendered = [' | '.join(row) for row in clean]
                        rendered.insert(1, ' | '.join('---' for _ in clean[0]))
                        text += '\n\n' + '\n'.join(rendered)
            if text:
                parts.append(f'## Стр. {number}\n\n{text}')
            page.close()  # Release pdfminer layout caches after each page.
    return '\n\n'.join(parts), needs_ocr
