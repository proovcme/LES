"""SafeRAG result and context policy."""

from __future__ import annotations

import logging
import re
from typing import Iterable, Protocol

logger = logging.getLogger(__name__)


class SourceChunk(Protocol):
    content: str
    doc_name: str


STOPWORDS = {
    "какая",
    "какие",
    "какой",
    "каким",
    "что",
    "это",
    "для",
    "или",
    "при",
    "над",
    "под",
    "если",
    "есть",
    "нужно",
    "нужен",
    "требуется",
    "применяется",
    "применяются",
    "регулируется",
    "относится",
    "относятся",
}

SAFE_FALLBACK = (
    "Система безопасности (Т.О.С.К.А.) не смогла подтвердить ответ из базы знаний. "
    "Попробуйте переформулировать вопрос или выбрать другой датасет."
)


def final_answer_for_status(answer: str, status: str) -> tuple[str, str]:
    if status in ("VERIFIED", "NO_DATA", "UNVALIDATED"):
        return answer, status
    if status in ("HALLUCINATION", "UNKNOWN"):
        return SAFE_FALLBACK, status
    return SAFE_FALLBACK, "UNKNOWN"


def query_terms(question: str) -> set[str]:
    return {
        token
        for token in re.findall(r"[0-9a-zа-яё]{3,}", question.casefold())
        if token not in STOPWORDS and len(token) >= 4
    }


def rank_chunks_for_question(
    question: str, chunks: list[SourceChunk], *, preserve_retrieval_order: bool = False,
) -> list[SourceChunk]:
    """Apply a tiny lexical boost on top of vector score for evidence ordering."""
    # The hybrid pipeline already owns ranking (RRF, optional cross-encoder,
    # explicit reference guards). Adding lexical scores here would undo it.
    if preserve_retrieval_order:
        return list(chunks)
    terms = query_terms(question)
    if not terms or not chunks:
        return chunks

    def rank_key(index_chunk: tuple[int, SourceChunk]) -> tuple[float, float, int]:
        index, chunk = index_chunk
        haystack = f"{chunk.doc_name}\n{chunk.content}".casefold()
        matches = sum(1 for term in terms if term in haystack)
        title_matches = sum(1 for term in terms if term in chunk.doc_name.casefold())
        score = float(getattr(chunk, "score", 0.0) or 0.0)
        boosted = float(getattr(chunk, "_rank_pin", 0.0) or 0.0) + score + matches * 0.12 + title_matches * 0.03
        try:
            setattr(chunk, "_rank_score", boosted)
        except Exception:
            pass
        return (boosted, score, -index)

    ranked = sorted(enumerate(chunks), key=rank_key, reverse=True)
    return [chunk for _, chunk in ranked]


def _doc_family(name: str) -> str:
    normalized = name.casefold()
    normalized = re.sub(r"\s+\(\d+\)(?=\.[^.]+$)", "", normalized)
    normalized = re.sub(r"\s+", " ", normalized)
    return normalized.strip()


def _content_fingerprint(chunk: SourceChunk) -> str:
    text = re.sub(r"\s+", " ", chunk.content.casefold()).strip()
    return text[:500]


def concentrate_sources(
    chunks: list[SourceChunk],
    max_docs: int = 2,
    min_score: float = 0.45,
    max_chunks: int | None = None,
    protected_doc_names: Iterable[str] | None = None,
) -> list[SourceChunk]:
    """
    Keep chunks from the most relevant documents to reduce context contamination.

    The score attribute is optional for compatibility with reranker fallback stubs.
    Missing scores are treated as relevant.
    `protected_doc_names` is an intent/evidence tier: callers may name documents
    that were deliberately opened (for example exact file-target retrieval). These
    documents still deduplicate and obey max_chunks, but are not dropped merely
    because semantic top-doc concentration preferred another family.
    """
    if not chunks:
        return chunks
    protected_docs = {_doc_family(str(name or "")) for name in (protected_doc_names or []) if str(name or "").strip()}

    def _focus_score(chunk: SourceChunk) -> float:
        return float(getattr(chunk, "_rank_score", getattr(chunk, "score", 0.0)) or 0.0)

    filtered = [c for c in chunks if _focus_score(c) >= min_score]
    if not filtered:
        best = max(_focus_score(c) for c in chunks)
        filtered = [c for c in chunks if _focus_score(c) >= best * 0.8]

    doc_max: dict[str, float] = {}
    for chunk in filtered:
        score = _focus_score(chunk)
        doc_key = _doc_family(chunk.doc_name)
        if doc_key not in doc_max or doc_max[doc_key] < score:
            doc_max[doc_key] = score

    top_docs = set(sorted(doc_max, key=lambda doc: -doc_max[doc])[:max_docs]) | protected_docs
    result = []
    seen_content: set[str] = set()
    for chunk in filtered:
        if _doc_family(chunk.doc_name) not in top_docs:
            continue
        fingerprint = _content_fingerprint(chunk)
        if fingerprint in seen_content:
            continue
        seen_content.add(fingerprint)
        result.append(chunk)
        if max_chunks is not None and len(result) >= max_chunks:
            break

    removed_docs = {chunk.doc_name for chunk in chunks if _doc_family(chunk.doc_name) not in top_docs}
    if removed_docs:
        logger.info("[FOCUS] Отсечено %s нерелевантных источников: %s", len(removed_docs), removed_docs)

    return result


def _source_label(index: int, chunk: SourceChunk, include_metadata: bool) -> str:
    if not include_metadata:
        return f"[{chunk.doc_name}]"
    meta = getattr(chunk, "meta", {}) or {}
    details = [f"Источник {index}", chunk.doc_name]
    # Ranking scores remain in diagnostics, not in model-facing evidence labels.
    page = meta.get("source_page") or meta.get("page") or meta.get("page_number")
    if page:
        details.append(f"стр. {page}")
    doc_type = meta.get("doc_type")
    if doc_type:
        details.append(str(doc_type))
    if meta.get("atomic_evidence") and meta.get("parent_id"):
        details.append("фрагмент раздела; полнота не гарантируется")
    return "[" + " | ".join(details) + "]"


_BR_RE = re.compile(r"<br\s*/?>", re.IGNORECASE)


def _clean_chunk_text(text: str) -> str:
    """Снять тег-суп `<br>` из деградированных табличных чанков (~18% корпуса: «215<br>1<br>5»,
    «**<br>**») перед подачей в LLM — модель не должна видеть HTML-мусор. Пайпы НЕ трогаем
    (можно сломать настоящую markdown-таблицу). No-op для чистого текста (дешёвый гард)."""
    if not text or "<br" not in text.lower():
        return text
    t = _BR_RE.sub(" ", text)
    t = re.sub(r"\*\*\s*\*\*", " ", t)   # осиротевшие ** после удаления <br>
    t = re.sub(r"[ \t]{2,}", " ", t)
    return t


_NUM_RUN_RE = re.compile(r"\d[\d  .,]{2,}\d")


def numeric_provenance_check(answer: str, context: str, *, max_flags: int = 5) -> list[str]:
    """Числа в ОТВЕТЕ (4+ значащих цифр), которых НЕТ в контексте — возможно не заземлённые
    (Codex §8, пет-гард, ТОЛЬКО метит). RAG не должен сам считать: число берётся из контекста.
    Нормализуем разделители (15 030,72 ↔ 15030.72). Годы (1900-2099) и короткие — пропускаем."""
    def _norm(s: str) -> str:
        return re.sub(r"[  .,]", "", s or "")

    ctx = _norm(context)
    flagged: list[str] = []
    for m in _NUM_RUN_RE.finditer(answer or ""):
        digits = re.sub(r"\D", "", m.group())
        if len(digits) < 4:
            continue
        if len(digits) == 4 and 1900 <= int(digits) <= 2099:   # год — не флагуем
            continue
        if digits not in ctx:
            flagged.append(m.group().strip())
        if len(flagged) >= max_flags:
            break
    return flagged


def _context_candidates(chunks: Iterable[SourceChunk]) -> list[tuple[int, SourceChunk]]:
    """Put one chunk per source first, then fill with remaining evidence."""
    items = list(enumerate(chunks))
    if any((getattr(chunk, "meta", {}) or {}).get("atomic_evidence") for _, chunk in items):
        return items
    first: list[tuple[int, SourceChunk]] = []
    rest: list[tuple[int, SourceChunk]] = []
    seen_docs: set[str] = set()
    for item in items:
        doc = _doc_family(str(getattr(item[1], "doc_name", "")))
        if doc not in seen_docs:
            seen_docs.add(doc)
            first.append(item)
        else:
            rest.append(item)
    return first + rest


def _visible_context_parts(
    chunks: Iterable[SourceChunk],
    max_chars: int,
    *,
    include_metadata: bool,
) -> list[tuple[int, int, SourceChunk, str, str]]:
    """Return exact visible evidence; an oversized chunk cannot block later ones."""
    visible: list[tuple[int, int, SourceChunk, str, str]] = []
    total = 0
    seen: set[tuple[str, str, str, str]] = set()
    for original_index, chunk in _context_candidates(chunks):
        visible_index = len(visible) + 1
        meta = getattr(chunk, "meta", {}) or {}
        clean = chunk.content if meta.get("atomic_evidence") else _clean_chunk_text(chunk.content)
        identity = (str(meta.get("dataset_id") or ""), str(chunk.doc_name),
                    str(meta.get("source_page") or meta.get("page") or ""), clean)
        if identity in seen:
            continue
        seen.add(identity)
        table_header = str(meta.get("table_header") or "").strip() if isinstance(meta, dict) else ""
        if table_header and not meta.get("atomic_evidence") and table_header not in clean[: max(len(table_header) + 40, 200)]:
            clean = f"[Заголовок таблицы] {table_header}\n{clean}"
        label = _source_label(visible_index, chunk, include_metadata)
        prefix = f"{label}:\n"
        remaining = max_chars - total - (2 if visible else 0)
        if len(prefix) + len(clean) > remaining:
            if meta.get("atomic_evidence"):
                continue
            # Do not spend a citation slot on an unusably tiny tail. Continue to
            # later (possibly shorter) evidence instead of stopping the pack.
            if remaining <= len(prefix) + 80:
                continue
            clean = clean[: max(0, remaining - len(prefix) - 1)].rstrip() + "…"
        part = prefix + clean
        if len(part) > remaining:
            continue
        visible.append((visible_index, original_index, chunk, clean, label))
        total += len(part) + (2 if visible_index > 1 else 0)
        if total >= max_chars:
            break
    return visible


def build_context(chunks: Iterable[SourceChunk], max_chars: int, *, include_metadata: bool = False) -> str:
    return "\n\n".join(
        f"{label}:\n{clean}"
        for _visible_index, _original_index, _chunk, clean, label in _visible_context_parts(
            chunks, max_chars, include_metadata=include_metadata
        )
    )


def source_map_for_context(
    chunks: Iterable[SourceChunk],
    max_chars: int,
    *,
    include_metadata: bool = False,
    snippet_chars: int = 220,
) -> list[dict[str, object]]:
    """Return the exact source numbering visible in ``build_context``.

    ``sources`` in chat responses are document chips, while the prompt cites chunk
    headers as "Источник N". This map keeps those two surfaces explainable.
    """
    out: list[dict[str, object]] = []
    for index, original_index, chunk, clean, label in _visible_context_parts(
        chunks, max_chars, include_metadata=include_metadata
    ):
        meta = getattr(chunk, "meta", {}) or {}
        item: dict[str, object] = {
            "index": index,
            "original_chunk_index": original_index,
            "label": f"Источник {index}",
            "doc_name": chunk.doc_name,
            "doc_id": str(getattr(chunk, "doc_id", "") or ""),
            "header": label,
            "snippet": clean[:snippet_chars].strip(),
        }
        score = getattr(chunk, "score", None)
        if isinstance(score, (int, float)):
            item["score"] = round(float(score), 3)
        if isinstance(meta, dict):
            for key in ("dataset_id", "source_page", "page", "page_number", "doc_type", "source_ref"):
                if meta.get(key):
                    item[key] = meta[key]
            if meta.get("atomic_evidence"):
                item["quote"] = clean
                for key in ("qdrant_point_id", "parent_id", "section_fragment_count", "context_origin"):
                    if meta.get(key) is not None:
                        item[key] = meta[key]
        out.append(item)
    return out


def build_validation_context(
    chunks: Iterable[SourceChunk],
    max_chars: int = 8000,
    *,
    include_metadata: bool = False,
) -> str:
    """Build the cited retrieval window passed to the validator."""
    return build_context(chunks, max_chars, include_metadata=include_metadata)


def source_names(chunks: Iterable[SourceChunk]) -> list[str]:
    names: list[str] = []
    seen: set[str] = set()
    for chunk in chunks:
        if chunk.doc_name in seen:
            continue
        names.append(chunk.doc_name)
        seen.add(chunk.doc_name)
    return names
