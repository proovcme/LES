"""
converter.py — конвертация документов в Markdown для RAG.

Поддерживаемые форматы:
  PDF, DOCX, EML/EMLX, MSG, XLSX/XLS/CSV, JSON/JSONL, MD, TXT
"""
import json
import io
import hashlib
import logging
import multiprocessing
import os
import queue
import re
import time
from pathlib import Path
from typing import Any, Optional
from backend.text_decoding import read_document_text

logger = logging.getLogger(__name__)

# Лимит текста на файл — защита от огромных документов
MAX_FILE_CHARS = 500_000  # ~125k токенов
PDF_MAX_FILE_CHARS = 2_000_000
PDF_FAST_TEXT_MAX_FILE_CHARS = 50_000_000
BOOK_PDF_MIN_PAGES = 200
SPREADSHEET_READ_ROWS = int(os.getenv("RAG_SPREADSHEET_READ_ROWS", "2000"))
SPREADSHEET_FULL_TABLE_MAX_CELLS = int(os.getenv("RAG_SPREADSHEET_FULL_TABLE_MAX_CELLS", "800"))
SPREADSHEET_PROFILE_MAX_COLUMNS = int(os.getenv("RAG_SPREADSHEET_PROFILE_MAX_COLUMNS", "60"))
SPREADSHEET_PROFILE_MAX_VALUES = int(os.getenv("RAG_SPREADSHEET_PROFILE_MAX_VALUES", "12"))
SPREADSHEET_SAMPLE_ROWS = int(os.getenv("RAG_SPREADSHEET_SAMPLE_ROWS", "8"))

IMAGE_SUFFIXES = {".jpg", ".jpeg", ".png", ".tif", ".tiff", ".bmp", ".webp"}
ISOLATED_CONVERT_SUFFIXES = {".pdf", ".p7m", ".xlsx", ".xlsm", ".xls"}

SUPPORTED = {
    ".pdf", ".docx", ".doc",
    ".eml", ".emlx", ".msg",
    ".xlsx", ".xlsm", ".xls", ".csv",
    ".pptx",
    ".json", ".jsonl",
    ".md", ".txt",
    ".p7m",                       # подписанный PKCS#7 (обычно PDF внутри) → разворачиваем
    *IMAGE_SUFFIXES,              # сканы-картинки → vision-OCR
}


def _parse_with_markitdown(file_path: Path) -> Optional[str]:
    """Конвертация с использованием универсального конвертера Microsoft MarkItDown."""
    try:
        from markitdown import MarkItDown
        md = MarkItDown()
        result = md.convert(str(file_path))
        if result and result.text_content:
            return result.text_content
    except Exception as e:
        logger.warning(f"[CONVERT] MarkItDown failed for {file_path.name}: {e}")
    return None


def convert_to_markdown(file_path: Path, route=None) -> Optional[str]:
    suffix = file_path.suffix.lower()
    if suffix not in SUPPORTED:
        logger.warning(f"[CONVERT] Неподдерживаемый формат: {suffix} ({file_path.name})")
        return None

    logger.info(f"[CONVERT] {file_path.name} ({suffix}, {file_path.stat().st_size // 1024} KB)")
    try:
        if suffix == ".pdf":
            result = _parse_pdf(file_path, route=route)
        elif suffix == ".p7m":
            result = _parse_p7m(file_path, route=route)
        elif suffix in IMAGE_SUFFIXES:
            result = _parse_image_ocr(file_path)
        elif suffix == ".docx":
            result = _parse_with_markitdown(file_path) or _parse_docx(file_path)
        elif suffix == ".doc":
            # legacy бинарный .doc: markitdown/mammoth (только .docx) не берут → нативный textutil
            result = _parse_with_markitdown(file_path) or _parse_legacy_doc(file_path)
        elif suffix in (".eml", ".emlx", ".msg"):
            result = _parse_email(file_path)
        elif suffix in (".xlsx", ".xlsm", ".xls", ".csv"):
            result = _parse_spreadsheet(file_path) or _parse_with_markitdown(file_path)
        elif suffix == ".pptx":
            result = _parse_with_markitdown(file_path)
        elif suffix in (".json", ".jsonl"):
            result = _parse_json(file_path)
        elif suffix in (".md", ".txt"):
            result = read_document_text(file_path)
        else:
            return None

        return _limit_file_chars(file_path, result)

    except Exception as e:
        logger.error(f"[CONVERT] Ошибка {file_path.name}: {e}", exc_info=True)
        return None


def convert_to_markdown_for_indexing(file_path: Path, route=None) -> Optional[str]:
    """Indexing entrypoint: risky PDF/Excel converters run in a killable child."""
    if not _isolated_conversion_enabled(file_path):
        return convert_to_markdown(file_path, route=route)
    if _pdf_index_fast_text_first(file_path, route=route):
        fast = _parse_pdf_fast_text_layer(file_path, reason="pdf_index_text_first")
        if fast and fast.strip():
            return _limit_file_chars(
                file_path,
                fast,
                env_name="RAG_PDF_FAST_TEXT_MAX_FILE_CHARS",
                default=PDF_FAST_TEXT_MAX_FILE_CHARS,
            )
    try:
        return convert_to_markdown_isolated(file_path, route=route)
    except RuntimeError as error:
        if _pdf_fast_text_fallback_enabled(file_path):
            fast = _parse_pdf_fast_text_layer(file_path, reason=f"isolated_convert_failed: {error}")
            if fast and fast.strip():
                logger.warning(
                    "[CONVERT] %s: isolated converter failed (%s), indexed fast page-text fallback",
                    file_path.name,
                    error,
                )
                return _limit_file_chars(
                    file_path,
                    fast,
                    env_name="RAG_PDF_FAST_TEXT_MAX_FILE_CHARS",
                    default=PDF_FAST_TEXT_MAX_FILE_CHARS,
                )
        raise


def convert_to_markdown_isolated(
    file_path: Path,
    route=None,
    timeout_sec: float | None = None,
) -> Optional[str]:
    timeout = timeout_sec if timeout_sec is not None else _isolated_conversion_timeout_sec()
    ctx = multiprocessing.get_context(os.getenv("RAG_CONVERT_PROCESS_START_METHOD", "spawn"))
    result_queue = ctx.Queue(maxsize=1)
    process = ctx.Process(
        target=_convert_to_markdown_worker,
        args=(str(file_path), route, result_queue),
        name=f"les-convert-{file_path.suffix.lower().lstrip('.') or 'file'}",
    )
    process.start()
    deadline = time.monotonic() + timeout
    result: tuple[str, Any] | None = None
    while process.is_alive() and time.monotonic() < deadline:
        try:
            result = result_queue.get(timeout=min(0.2, max(0.01, deadline - time.monotonic())))
            break
        except queue.Empty:
            continue

    if result is not None:
        process.join(5)
    elif process.is_alive():
        process.terminate()
        process.join(5)
        if process.is_alive():
            process.kill()
            process.join(5)
        raise RuntimeError(f"convert subprocess timeout: >{timeout:.0f}s")
    else:
        process.join(5)
        try:
            result = result_queue.get_nowait()
        except queue.Empty:
            if process.exitcode == 0:
                return None
            raise RuntimeError(f"convert subprocess exited with code {process.exitcode}")

    status, payload = result
    if status == "ok":
        return payload
    raise RuntimeError(str(payload))


def _convert_to_markdown_worker(file_path: str, route: Any, result_queue: Any) -> None:
    try:
        result_queue.put(("ok", convert_to_markdown(Path(file_path), route=route)))
    except BaseException as error:  # noqa: BLE001 - cross-process boundary
        result_queue.put(("error", f"{type(error).__name__}: {error}"))


def _isolated_conversion_enabled(file_path: Path) -> bool:
    raw = os.getenv("RAG_CONVERT_SUBPROCESS_ENABLED", "true").strip().lower()
    if raw not in {"1", "true", "yes", "on"}:
        return False
    return file_path.suffix.lower() in ISOLATED_CONVERT_SUFFIXES


def _isolated_conversion_timeout_sec() -> float:
    raw = os.getenv("RAG_CONVERT_SUBPROCESS_TIMEOUT_SEC")
    if raw:
        try:
            return max(1.0, float(raw))
        except ValueError:
            pass
    try:
        parse_timeout = float(os.getenv("RAG_PARSE_FILE_TIMEOUT_SEC", "1800"))
    except ValueError:
        parse_timeout = 1800.0
    return max(1.0, parse_timeout * 0.9)


def _env_bool(name: str, default: bool) -> bool:
    raw = os.getenv(name)
    if raw is None:
        return default
    return raw.strip().lower() in {"1", "true", "yes", "on"}


def _pdf_fast_text_fallback_enabled(file_path: Path) -> bool:
    return file_path.suffix.lower() == ".pdf" and _env_bool("RAG_PDF_FAST_TEXT_FALLBACK_ENABLED", True)


def _pdf_index_fast_text_first(file_path: Path, route=None) -> bool:
    if file_path.suffix.lower() != ".pdf":
        return False
    if not _env_bool("RAG_PDF_INDEX_FAST_TEXT_FIRST", True):
        return False
    if route and getattr(route, "pipeline", None) == "markdown_needs_ocr":
        return False
    page_count = _pdf_page_count(file_path)
    try:
        min_pages = max(1, int(os.getenv("RAG_PDF_FAST_TEXT_FIRST_MIN_PAGES", "1")))
    except ValueError:
        min_pages = 30
    try:
        min_mb = max(0.1, float(os.getenv("RAG_PDF_FAST_TEXT_FIRST_MIN_MB", "1")))
    except ValueError:
        min_mb = 15.0
    size_mb = file_path.stat().st_size / (1024 * 1024) if file_path.exists() else 0.0
    pipeline = str(getattr(route, "pipeline", "") or "")
    return page_count >= min_pages or size_mb >= min_mb or pipeline == "markdown_pdf_tables"


def _parse_pdf_fast_text_layer(path: Path, *, reason: str = "") -> Optional[str]:
    from .pdf_reader import extract_markdown
    try:
        text, scans = extract_markdown(path, tables=False)
        return normalize_pdf_text(text) if text and not scans else None
    except Exception as error:
        logger.warning("[CONVERT] PDF text extraction failed: %s", error)
        return None


def _parse_pdf(path: Path, route=None) -> Optional[str]:
    from .pdf_reader import extract_markdown
    text, scans = extract_markdown(path)
    if scans or (route and getattr(route, 'pipeline', '') == 'markdown_needs_ocr'):
        if not _env_bool('RAG_OCR_ENABLED', True):
            return None
        try:
            from .ocr_parser import make_ocr_parser
            text = make_ocr_parser().parse_pdf(path, dpi=int(os.getenv('RAG_OCR_DPI', '150')))
        except Exception as error:
            logger.error('[CONVERT] OCR failed for %s: %s', path.name, error)
            return None
    return strip_legal_boilerplate(normalize_pdf_text(text)) if text.strip() else None


# Колонтитулы правовых систем («КонсультантПлюс … Страница N из M») превращаются
# конвертацией в заголовки-чанки и засоряют выдачу с высокими скорами (кейс
# Постановления 87, 2026-06-14). Чистим детерминированно (ADR-11).
_BOILERPLATE_LINE_RE = re.compile(
    r"^\s*#{0,6}\s*\**\s*("
    r"КонсультантПлюс|www\.consultant\.ru|consultant\.ru|"
    r"Страница\s+\d+\s+из\s+\d+|"
    r"надежная правовая поддержка|"
    r"Документ предоставлен КонсультантПлюс"
    r")[\s.*]*$",
    re.IGNORECASE | re.MULTILINE,
)


def strip_legal_boilerplate(md: str) -> str:
    """Удаляет строки-колонтитулы правовых систем из markdown."""
    if not md:
        return md
    cleaned = _BOILERPLATE_LINE_RE.sub("", md)
    return re.sub(r"\n{4,}", "\n\n\n", cleaned)


def normalize_pdf_text(text: str) -> str:
    """Repair UTF-8 Cyrillic accidentally exposed as Latin-1 by Windows PDF parsers."""
    raw = str(text or "")
    if not raw or not any(marker in raw for marker in ("Ð", "Ñ", "Â")):
        return raw

    def _repair_run(match: re.Match[str]) -> str:
        run = match.group(0)
        try:
            candidate = run.encode("latin-1").decode("utf-8")
        except (UnicodeEncodeError, UnicodeDecodeError):
            return run
        candidate_cyr = sum("А" <= ch <= "я" or ch in "Ёё" for ch in candidate)
        return candidate if candidate_cyr else run

    # Repair only high-byte runs. Valid punctuation such as the middle dot may
    # coexist with broken Cyrillic and is not itself a valid UTF-8 byte sequence.
    repaired = re.sub(r"[\x80-\xff]+", _repair_run, raw)
    raw_noise = sum(raw.count(marker) for marker in ("Ð", "Ñ", "Â"))
    repaired_noise = sum(repaired.count(marker) for marker in ("Ð", "Ñ", "Â"))
    raw_cyr = sum("А" <= ch <= "я" or ch in "Ёё" for ch in raw)
    repaired_cyr = sum("А" <= ch <= "я" or ch in "Ёё" for ch in repaired)
    return repaired if repaired_noise < raw_noise and repaired_cyr > raw_cyr else raw


_docling_converter = None  # ленивая инициализация: тяжёлые layout-модели грузятся один раз


def _docling_pdf_markdown(path: Path) -> str:
    """W1.5: PDF → markdown через Docling (layout-aware, TableFormer для таблиц)."""
    global _docling_converter
    if _docling_converter is None:
        from docling.document_converter import DocumentConverter

        _docling_converter = DocumentConverter()
        logger.info("[CONVERT] docling инициализирован (первый вызов грузит модели)")
    result = _docling_converter.convert(str(path))
    return result.document.export_to_markdown()


def _max_file_chars(path: Path) -> int:
    if path.suffix.lower() == ".pdf" and _pdf_page_count(path) >= BOOK_PDF_MIN_PAGES:
        default = PDF_MAX_FILE_CHARS
    else:
        default = MAX_FILE_CHARS
    env_name = "RAG_PDF_MAX_FILE_CHARS" if path.suffix.lower() == ".pdf" else "RAG_MAX_FILE_CHARS"
    try:
        return max(1, int(os.getenv(env_name, str(default))))
    except ValueError:
        return default


def _limit_file_chars(
    file_path: Path,
    result: Optional[str],
    *,
    env_name: str | None = None,
    default: int | None = None,
) -> Optional[str]:
    if env_name:
        try:
            max_chars = max(1, int(os.getenv(env_name, str(default or _max_file_chars(file_path)))))
        except ValueError:
            max_chars = default or _max_file_chars(file_path)
    else:
        max_chars = _max_file_chars(file_path)
    if result and len(result) > max_chars:
        logger.warning(f"[CONVERT] {file_path.name}: обрезан до {max_chars} символов")
        result = result[:max_chars]
    return result if result and result.strip() else None


def _pdf_image_extraction_enabled(path: Path) -> bool:
    raw = os.getenv("PDF_IMAGE_EXTRACTION_ENABLED")
    if raw is not None:
        return raw.lower() in {"1", "true", "yes", "on"}
    return _pdf_page_count(path) >= BOOK_PDF_MIN_PAGES


def _pdf_page_count(path: Path) -> int:
    try:
        import pdfplumber

        with pdfplumber.open(path) as doc:
            return len(doc.pages)
    except Exception:
        return 0


def _pdf_image_dir(path: Path) -> Path:
    from backend.rag_config import rag_meta_db_path
    identity = hashlib.sha256(str(path.resolve()).encode('utf-8')).hexdigest()
    image_dir = Path(rag_meta_db_path()).resolve().parent / 'document-assets' / identity
    image_dir.mkdir(parents=True, exist_ok=True)
    return image_dir


def _parse_docx(path: Path) -> str:
    import mammoth
    with open(path, "rb") as f:
        result = mammoth.convert_to_markdown(f)
    if result.messages:
        for msg in result.messages:
            logger.debug(f"[CONVERT] mammoth: {msg}")
    return result.value


def _parse_legacy_doc(path: Path) -> Optional[str]:
    """Бинарный .doc (Word 97-2003): mammoth/markitdown их не читают (только .docx).

    macOS-нативный ``textutil`` конвертирует .doc/.rtf в txt без сторонних зависимостей.
    Если textutil недоступен (не macOS) — мягко вернуть None (фолбэк на antiword/catdoc, если есть).
    """
    import shutil
    import subprocess

    tu = shutil.which("textutil")
    if tu:
        try:
            out = subprocess.run([tu, "-convert", "txt", "-stdout", str(path)],
                                 capture_output=True, timeout=60)
            text = out.stdout.decode("utf-8", errors="ignore").strip()
            if text:
                return text
            logger.warning("[CONVERT] textutil вернул пусто для %s", path.name)
        except Exception as err:  # noqa: BLE001
            logger.warning("[CONVERT] textutil не справился с %s: %s", path.name, err)
    for tool in ("antiword", "catdoc"):  # фолбэк для не-macOS, если установлены
        exe = shutil.which(tool)
        if exe:
            try:
                out = subprocess.run([exe, str(path)], capture_output=True, timeout=60)
                text = out.stdout.decode("utf-8", errors="ignore").strip()
                if text:
                    return text
            except Exception:  # noqa: BLE001
                continue
    return None


def _parse_image_ocr(path: Path) -> Optional[str]:
    """Скан-картинка (jpg/png/tiff) → текст через тот же vision-OCR, что и скан-PDF."""
    if os.getenv("RAG_OCR_ENABLED", "true").lower() not in ("true", "1", "yes", "on"):
        return None
    try:
        from PIL import Image

        from .ocr_parser import make_ocr_parser

        parser = make_ocr_parser()
        with Image.open(path) as img:
            text = parser.ocr_page(img.convert("RGB"))
        return text if text and text.strip() else None
    except Exception as err:  # noqa: BLE001 — OCR не должен ронять индексацию
        logger.error("[CONVERT] image-OCR %s: %s", path.name, err)
        return None


def _parse_p7m(path: Path, route=None) -> Optional[str]:
    """Подписанный контейнер PKCS#7 (.p7m, обычно PDF внутри): развернуть openssl → распарсить.

    `openssl smime -verify -noverify` снимает подпись без проверки цепочки (нам нужен контент,
    не валидация). Пробуем DER и PEM. Развёрнутый PDF идёт штатным PDF-путём (текст/OCR).
    """
    import shutil
    import subprocess
    import tempfile

    openssl = shutil.which("openssl")
    if not openssl:
        logger.warning("[CONVERT] openssl недоступен — .p7m %s пропущен", path.name)
        return None
    # «Лесной64_АС.pdf.p7m» рядом с «Лесной64_АС.pdf» → открепленная подпись: контента нет,
    # оригинал индексируется сам — тихо пропускаем.
    sibling = path.with_suffix("")  # снять .p7m
    detached = sibling.exists() and sibling.suffix.lower() in SUPPORTED
    with tempfile.TemporaryDirectory() as td:
        out = Path(td) / "content.bin"
        # cms — современный путь для CAdES/PKCS#7; smime — запасной; оба формата DER/PEM
        attempts = [[openssl, "cms", "-verify", "-noverify", "-in", str(path), "-inform", inf, "-out", str(out)]
                    for inf in ("DER", "PEM")]
        attempts += [[openssl, "smime", "-verify", "-noverify", "-in", str(path), "-inform", inf, "-out", str(out)]
                     for inf in ("DER", "PEM")]
        for cmd in attempts:
            try:
                subprocess.run(cmd, capture_output=True, timeout=60)
                if out.exists() and out.stat().st_size > 0:
                    if out.read_bytes()[:5].startswith(b"%PDF"):
                        pdf = out.with_suffix(".pdf"); out.rename(pdf)
                        return _parse_pdf(pdf, route=route)
                    txt = read_document_text(out).strip()
                    if txt:
                        return txt
            except Exception:  # noqa: BLE001
                continue
    if detached:
        logger.info("[CONVERT] .p7m %s — открепленная подпись (оригинал %s индексируется отдельно)",
                    path.name, sibling.name)
    else:
        logger.warning("[CONVERT] .p7m %s: не удалось развернуть контейнер", path.name)
    return None


def _parse_email(path: Path) -> str:
    from .mail_profile import build_mail_vector_profile

    profile = build_mail_vector_profile(path)
    return profile.message_embedding_text(include_attachment_text=True)


def _parse_spreadsheet(path: Path) -> str:
    import pandas as pd
    md_parts = []

    try:
        if path.suffix.lower() == ".csv":
            df = pd.read_csv(io.StringIO(read_document_text(path)), nrows=SPREADSHEET_READ_ROWS)
            md_parts.append(_render_spreadsheet_sheet(path.stem, df))
        else:
            xls = pd.ExcelFile(path)
            for sheet in xls.sheet_names:
                df = pd.read_excel(xls, sheet_name=sheet, nrows=SPREADSHEET_READ_ROWS)
                if df.empty:
                    continue
                md_parts.append(_render_spreadsheet_sheet(sheet, df))
    except Exception as e:
        logger.error(f"[CONVERT] spreadsheet error {path.name}: {e}")
        return f"[ERROR] Не удалось прочитать таблицу: {e}"

    return "\n\n".join(md_parts) if md_parts else f"[WARN] {path.name}: таблица пуста"


def _render_spreadsheet_sheet(sheet_name: str, df: Any) -> str:
    """Render spreadsheet data for navigation RAG.

    Small sheets stay as full markdown tables. Large sheets become compact
    structural projections so a workbook cannot dominate the vector index with
    thousands of near-identical row chunks. Exact rows/sums must be read by a
    table reader/tool from the source file.
    """
    cleaned = df.dropna(how="all").dropna(axis=1, how="all")
    if cleaned.empty:
        return f"## Лист: {sheet_name}\n[WARN] лист пуст"

    rows, cols = cleaned.shape
    if rows * cols <= SPREADSHEET_FULL_TABLE_MAX_CELLS:
        return f"## Лист: {sheet_name}\n{cleaned.to_markdown(index=False)}"

    parts = [
        f"## Лист: {sheet_name}",
        "Тип: spreadsheet_navigation_projection",
        f"Размер прочитанного окна: строк {rows}, колонок {cols}",
        "Назначение: навигация по книге и выбор листа/колонок; точные строки и расчеты читать из исходного файла табличным reader/tool.",
        "",
        "### Колонки",
        ", ".join(_safe_cell_text(col, max_len=80) for col in cleaned.columns),
    ]
    if rows >= SPREADSHEET_READ_ROWS:
        parts.append(f"Примечание: прочитано первые {SPREADSHEET_READ_ROWS} строк; исходный лист может быть длиннее.")

    profiles = []
    for col in list(cleaned.columns)[:SPREADSHEET_PROFILE_MAX_COLUMNS]:
        series = cleaned[col].dropna()
        if series.empty:
            continue
        profile = _spreadsheet_column_profile(col, series)
        if profile:
            profiles.append(profile)
    if profiles:
        parts.extend(["", "### Профили колонок", *profiles])

    samples = cleaned.head(SPREADSHEET_SAMPLE_ROWS)
    if not samples.empty:
        parts.extend(["", "### Образец строк", samples.to_markdown(index=False)])

    return "\n".join(parts)


def _spreadsheet_column_profile(col: Any, series: Any) -> str:
    import pandas as pd

    name = _safe_cell_text(col, max_len=80)
    numeric = pd.to_numeric(series, errors="coerce").dropna()
    bits = [f"- {name}: заполнено {int(series.shape[0])}"]
    if len(numeric) >= max(3, int(series.shape[0] * 0.5)):
        bits.append(
            "числа "
            f"min={_safe_number(numeric.min())}, "
            f"max={_safe_number(numeric.max())}, "
            f"sum={_safe_number(numeric.sum())}"
        )
    values = []
    seen = set()
    for value in series:
        text = _safe_cell_text(value)
        if not text or text in seen:
            continue
        seen.add(text)
        values.append(text)
        if len(values) >= SPREADSHEET_PROFILE_MAX_VALUES:
            break
    if values:
        bits.append("примеры: " + "; ".join(values))
    return " · ".join(bits)


def _safe_cell_text(value: Any, max_len: int = 120) -> str:
    text = str(value).replace("\n", " ").strip()
    text = re.sub(r"\s+", " ", text)
    if text.lower() == "nan":
        return ""
    return text[:max_len]


def _safe_number(value: Any) -> str:
    try:
        number = float(value)
    except Exception:
        return str(value)
    if number.is_integer():
        return str(int(number))
    return f"{number:.3f}".rstrip("0").rstrip(".")


def _parse_json(path: Path) -> str:
    md = []
    size_mb = path.stat().st_size / (1024 * 1024)
    MAX_ENTRIES = 2000

    try:
        with io.StringIO(read_document_text(path)) as f:
            first_line = f.readline().strip()
            second_line = ""
            for _l in f:                      # следующая НЕПУСТАЯ строка (readline уже съел первую)
                if _l.strip():
                    second_line = _l.strip()
                    break
            f.seek(0)

            # JSONL = расширение .jsonl ЛИБО есть ВТОРАЯ строка, тоже парсящаяся как JSON.
            # Компактный json-массив (json.dumps([...]) — ОДНА строка) → second_line="" → НЕ jsonl,
            # читаем целиком (иначе весь list уходил в построчную ветку → "" → файл индексировался ПУСТЫМ).
            is_jsonl = path.suffix.lower() == ".jsonl"
            if not is_jsonl and second_line:
                try:
                    json.loads(first_line)
                    json.loads(second_line)
                    is_jsonl = True
                except Exception:
                    pass

            if is_jsonl or (size_mb > 10 and bool(second_line)):
                # Стриминг построчно
                for i, line in enumerate(f):
                    if i >= MAX_ENTRIES:
                        md.append(f"*[обрезано после {MAX_ENTRIES} записей]*")
                        break
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        obj = json.loads(line)
                        txt = _extract_json_text(obj)
                        if txt:
                            md.append(f"### Запись {i+1}\n{txt}")
                    except Exception:
                        pass
            else:
                data = json.load(f)
                items = data if isinstance(data, list) else [data]
                for i, item in enumerate(items[:MAX_ENTRIES]):
                    txt = _extract_json_text(item)
                    if txt:
                        md.append(f"### Запись {i+1}\n{txt}")

    except Exception as e:
        return f"[ERROR] JSON parse failed: {e}"

    return "\n\n".join(md) if md else f"[WARN] {path.name}: нет извлекаемого текста"


# Поля которые содержат полезный текст в типичных JSON датасетах
_TEXT_KEYS = frozenset([
    "role", "user", "assistant", "system", "prompt", "response",
    "content", "message", "text", "subject", "body", "delta",
    "question", "answer", "title", "description", "summary",
])

def _extract_json_text(obj, depth: int = 0) -> str:
    """Рекурсивно извлекает текст из JSON-объекта (глубина до 2)."""
    if depth > 2:
        return ""
    if isinstance(obj, str):
        return obj[:2000] if len(obj) > 5 else ""
    if not isinstance(obj, dict):
        return ""
    parts = []
    for k, v in obj.items():
        k_lower = k.lower()
        if k_lower in _TEXT_KEYS:
            if isinstance(v, str) and len(v) > 5:
                parts.append(f"**{k}:** {v[:2000]}")
            elif isinstance(v, dict):
                nested = _extract_json_text(v, depth + 1)
                if nested:
                    parts.append(nested)
    return "\n".join(parts)
