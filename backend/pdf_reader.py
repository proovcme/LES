"""PDF text and bounded rendering without process-global PDFium races."""
from __future__ import annotations

import math
import logging
import os
from pathlib import Path
from threading import RLock

# PDFium is not thread safe, including calls on separate documents. Keep all
# native object lifetimes inside this lock; return an independent PIL image.
_PDFIUM_LOCK = RLock()
logger = logging.getLogger(__name__)


def _native_worker_result(path: Path, *, timeout_ms: int = 30_000, **options) -> dict | None:
    from backend import native_core

    worker = os.getenv("LES_NATIVE_PDF_WORKER")
    library = os.getenv("LES_NATIVE_PDFIUM")
    if not worker or not library or not native_core.supports("worker_pdf"):
        return None
    response = native_core.required("worker_pdf", request={
        "schema": "les.worker-host.v1", "program": worker, "directory": str(Path(worker).parent),
        "args": ["--serve-pdf-text", library], "context": "les.pdf-text.v1", "timeout_ms": timeout_ms,
        "input": {"path": str(path.resolve()), **options},
    })
    if response.get("diagnostics") and options.get("ocr"):
        logger.info("PDF OCR worker: %s", str(response["diagnostics"])[-3000:])
    return response["result"] if response["status"] == "completed" else response


def _native_markdown(path: Path) -> tuple[str, bool] | None:
    from backend import native_core

    try:
        result = _native_worker_result(path)
        if result is None:
            return None
        value = result.get("value")
        if (result.get("status") == "completed" and isinstance(value, dict)
                and isinstance(value.get("text"), str) and isinstance(value.get("needs_ocr"), bool)):
            return value["text"], value["needs_ocr"]
        logger.info("PDF_NATIVE_COMPAT code=%s", result.get("code", "PDF_RESPONSE_INVALID"))
    except (OSError, native_core.NativeCoreError) as error:
        logger.warning("PDF_NATIVE_COMPAT type=%s", type(error).__name__)
    return None


def read_source_page(database: str, source: dict) -> dict:
    """Rust resolves catalog identity, verifies the revision and reads one PDF page."""
    from backend import native_core
    from backend.runtime_paths import mutable_path

    worker = os.getenv('LES_NATIVE_PDF_WORKER')
    library = os.getenv('LES_NATIVE_PDFIUM')
    if not worker or not library:
        return {'status': 'failed', 'code': 'PDF_NATIVE_UNAVAILABLE'}
    response = native_core.required('worker_pdf', request={
        'schema': 'les.worker-host.v1', 'program': worker, 'directory': str(Path(worker).parent),
        'args': ['--serve-pdf-text', library], 'context': 'les.pdf-text.v1', 'timeout_ms': 30_000,
        'input': {'kind': 'source_page', 'database': str(Path(database).resolve()), 'source': source,
                  'content_root': str(mutable_path('storage/datasets').resolve())},
    })
    return response['result'] if response.get('status') == 'completed' else {
        'status': 'failed', 'code': 'PDF_WORKER_UNAVAILABLE'}


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
    if not tables:
        result = _native_markdown(path)
        if result is not None:
            return result
    parts = []
    needs_ocr = False
    for number, text, scan in text_pages(path):
        needs_ocr |= scan
        if text:
            parts.append(f'## Стр. {number}\n\n{text}')
    if tables:
        import pdfplumber
        import logging
        deferred = complex_geometry_pages(path)
        if deferred:
            logging.getLogger(__name__).warning('PDF_TABLE_GEOMETRY_LIMIT document; text retained')
            return '\n\n'.join(parts), needs_ocr
        with pdfplumber.open(path) as document:
            for number, page in enumerate(document.pages, 1):
                try:
                    if len(page.edges) > 2000:
                        logging.getLogger(__name__).warning('PDF_TABLE_GEOMETRY_LIMIT page=%s; text retained', number)
                        continue
                    for rows in page.extract_tables() or []:
                        clean = [[str(cell or '').replace('\n', ' ').replace('|', '\\|') for cell in row] for row in rows]
                        if clean:
                            rendered = [' | '.join(row) for row in clean]
                            rendered.insert(1, ' | '.join('---' for _ in clean[0]))
                            parts.append(f'## Стр. {number}\n\n' + '\n'.join(rendered))
                finally:
                    page.close()
    return '\n\n'.join(parts), needs_ocr


def text_pages(path: Path, *, limit: int | None = None):
    """Bounded PDFium page lifetimes, also for image-only and outlined drawings."""
    import pypdfium2 as pdfium
    with _PDFIUM_LOCK, pdfium.PdfDocument(str(path)) as document:
        count = len(document) if limit is None else min(len(document), limit)
        for index in range(count):
            page = document[index]
            textpage = None
            try:
                textpage = page.get_textpage()
                text = textpage.get_text_range().strip()
                scan = False
                if len(text) < 20:
                    # Empty TEXT operators are not a scan. Preserve short native
                    # text on vector sheets (e.g. separator page numbers); OCR
                    # must not replace it with an empty result. Images still
                    # require OCR even when accompanied by a short page number.
                    for obj in page.get_objects(max_depth=15):
                        if obj.type == pdfium.raw.FPDF_PAGEOBJ_IMAGE or (
                            not text and obj.type == pdfium.raw.FPDF_PAGEOBJ_PATH
                        ):
                            scan = True
                            break
            finally:
                if textpage is not None:
                    textpage.close()
                page.close()
            yield index + 1, text, scan


def complex_geometry_pages(path: Path | bytes, *, pages=None, limit: int = 2000) -> set[int]:
    """Check native object/segment counts before allocating pdfminer layouts."""
    import pypdfium2 as pdfium
    deferred = set()
    with _PDFIUM_LOCK, pdfium.PdfDocument(path if isinstance(path, bytes) else str(path)) as document:
        selected = range(1, len(document)+1) if pages is None else sorted(set(pages))
        for number in selected:
            if not 1 <= number <= len(document):
                continue
            page = document[number-1]
            try:
                complexity = 0
                for obj in page.get_objects(max_depth=15):
                    complexity += 1
                    if obj.type == pdfium.raw.FPDF_PAGEOBJ_PATH:
                        complexity += max(0, pdfium.raw.FPDFPath_CountSegments(obj.raw))
                    if complexity > limit:
                        deferred.add(number)
                        break
            finally:
                page.close()
    return deferred
