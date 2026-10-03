"""Answer body rendering and citation anchors, independent of chat orchestration."""
import json
import re
from nicegui import ui

def _format_sources_as_quotes(text: str) -> str:
    """Keep prose readable: source service lines are replaced by links below the answer."""
    from sovushka.answer_render import normalize_inline_math, split_inline_source_notes

    body, _notes = split_inline_source_notes(text)
    return normalize_inline_math(body)

def _source_anchor_prefix(meta: dict | None) -> str:
    history_id = (meta or {}).get("history_id") if isinstance(meta, dict) else None
    try:
        return f"source-{int(history_id)}" if history_id is not None else f"source-current-{id(meta)}"
    except (TypeError, ValueError):
        return f"source-current-{id(meta)}"

def _link_visible_sources(text: str, srcs: list, meta: dict | None) -> str:
    from sovushka.answer_render import citation_sources, link_source_markers

    effective = citation_sources(
        srcs,
        (meta or {}).get("source_map") if isinstance(meta, dict) else None,
    )
    return link_source_markers(
        _format_sources_as_quotes(text),
        source_count=len(effective),
        anchor_prefix=_source_anchor_prefix(meta),
        sources=effective,
    )

from sovushka.components.source_links import source_markdown as _source_markdown

# ── Богатые формы ПРЯМО В ЧАТЕ (таблицы/mermaid → красиво, не сырой текст) ──
# Ответ режется на сегменты по месту блока (mermaid-fence, markdown-таблица),
# проза остаётся прозой. Каждый сегмент рисуется своим виджетом: text → ui.markdown,
# table → ui.table, mermaid → ui.mermaid (NiceGUI бандлит mermaid.js, рисует SVG).
# Fenced-блоки: ```mermaid``` → диаграмма, ```json [ {..} ]``` → таблица.
_BLOCK_RE = re.compile(
    r"```mermaid\s*(?P<mermaid>.*?)```|```json\s*(?P<json>\[\s*\{.*?\}\s*\])\s*```",
    re.DOTALL | re.IGNORECASE,
)

def _parse_md_table(block_lines: list[str]):
    """Блок строк md-таблицы (| a | b | + строка-разделитель ---) → list[dict]."""
    if len(block_lines) < 2:
        return None
    if not re.match(r"^\s*\|?[\s:|-]+\|?\s*$", block_lines[1]):
        return None

    def _cells(row: str) -> list[str]:
        return [c.strip() for c in row.strip().strip("|").split("|")]

    headers = _cells(block_lines[0])
    rows: list[dict] = []
    for row in block_lines[2:]:
        vals = _cells(row)
        if not any(vals):
            continue
        vals = (vals + [""] * len(headers))[: len(headers)]
        rows.append({h or f"col{i+1}": vals[i] for i, h in enumerate(headers)})
    return rows or None

def _md_table_row(line: str) -> str | None:
    """Строка md-таблицы (терпим ведущий маркер списка «- | … |», которым модель
    иногда оборачивает таблицу) → нормализованная «| … |», иначе None."""
    s = re.sub(r"^[-*•]\s+", "", line.strip())
    return s if (s.startswith("|") and s.count("|") >= 2) else None

def _md_table_at(lines: list[str], i: int):
    """Если со строки i начинается md-таблица — (rows, next_i), иначе None."""
    block: list[str] = []
    j = i
    while j < len(lines):
        s = _md_table_row(lines[j])
        if s is not None:
            block.append(s)
            j += 1
        else:
            break
    rows = _parse_md_table(block)
    return (rows, j) if rows else None

def _segment_text_and_tables(chunk: str) -> list[dict]:
    """Текстовый кусок (без mermaid) → проза + md-таблицы как отдельные сегменты."""
    lines = chunk.splitlines()
    out: list[dict] = []
    buf: list[str] = []
    i = 0

    def _flush():
        txt = "\n".join(buf).strip()
        if txt:
            out.append({"kind": "text", "text": txt})
        buf.clear()

    while i < len(lines):
        if _md_table_row(lines[i]) is not None:
            tbl = _md_table_at(lines, i)
            if tbl:
                _flush()
                rows, nxt = tbl
                out.append({"kind": "table", "rows": rows})
                i = nxt
                continue
        buf.append(lines[i])
        i += 1
    _flush()
    return out

def _segment_answer(ans: str) -> list[dict]:
    """Ответ → последовательность сегментов text / table / mermaid.

    Ловит fenced-блоки (```mermaid```, ```json [..]```), прочее отдаёт в
    _segment_text_and_tables (проза + markdown-таблицы)."""
    ans = ans or ""
    segments: list[dict] = []
    cursor = 0
    for m in _BLOCK_RE.finditer(ans):
        pre = ans[cursor:m.start()]
        if pre.strip():
            segments.extend(_segment_text_and_tables(pre))
        if m.group("mermaid") is not None:
            code = (m.group("mermaid") or "").strip()
            if code:
                segments.append({"kind": "mermaid", "code": code})
        elif m.group("json") is not None:
            try:
                data = json.loads(m.group("json"))
            except Exception:
                data = None
            if isinstance(data, list) and data and isinstance(data[0], dict):
                segments.append({"kind": "table", "rows": data})
            else:  # не таблица — оставить как код в прозе
                segments.append({"kind": "text", "text": m.group(0)})
        cursor = m.end()
    tail = ans[cursor:]
    if tail.strip():
        segments.extend(_segment_text_and_tables(tail))
    return segments

def _render_inline_table(rows: list[dict]) -> None:
    """Красивая таблица в пузыре чата (ui.table: сортировка, лёгкий стиль).
    v0.16: снимаем inline-markdown с ячеек (`**Тип**`→`Тип`), числовые колонки — вправо."""
    from sovushka.answer_render import clean_table_rows, strip_markdown_cell
    rows = clean_table_rows(rows)
    keys = list(rows[0].keys()) if rows else []

    def _numeric(k):
        vals = [str(r.get(k, "")).replace(",", ".").replace(" ", "") for r in rows]
        ok = [v for v in vals if v]
        return bool(ok) and all(re.fullmatch(r"-?\d+(\.\d+)?", v or "") for v in ok)

    cols = [{"name": k, "label": strip_markdown_cell(k), "field": k,
             "align": "right" if _numeric(k) else "left", "sortable": True} for k in keys]
    with ui.element("div").classes("sov-table-scroll"):
        ui.table(columns=cols, rows=rows, pagination={"rowsPerPage": 0}).props("dense flat bordered").classes(
            "sov-chat-inline-table"
        )

def _render_inline_mermaid(code: str) -> None:
    """Mermaid → SVG прямо в чате. ui.mermaid рисует SVG на клиенте; если упал —
    fallback на исходный код в свёрнутом блоке (чтобы данные не пропали)."""
    try:
        ui.mermaid(code).classes("sov-chat-inline-mermaid w-full")
    except Exception:
        with ui.expansion("Диаграмма (код Mermaid)", icon="account_tree").props("dense").classes("w-full"):
            ui.markdown(f"```mermaid\n{code}\n```").classes("sov-chat-message-text")

def _render_rich_body(text: str, srcs: list | None = None, meta: dict | None = None) -> bool:
    """Отрисовать тело ответа богато (таблицы/mermaid внутри прозы). True — если
    нарисован хоть один не-текстовый сегмент (тогда сырой ui.label не нужен)."""
    ans = str(text or "")
    try:
        segments = _segment_answer(ans)
    except Exception:
        return False
    if not any(seg["kind"] != "text" for seg in segments):
        return False
    linked_answer = _link_visible_sources(ans, srcs or [], meta)
    if any(seg["kind"] == "table" for seg in segments) and linked_answer != _format_sources_as_quotes(ans):
        _source_markdown(linked_answer).classes("sov-chat-message-text sov-chat-md")
        return True
    with ui.column().classes("sov-chat-rich w-full gap-2"):
        for seg in segments:
            if seg["kind"] == "table":
                _render_inline_table(seg["rows"])
            elif seg["kind"] == "mermaid":
                _render_inline_mermaid(seg["code"])
            else:
                _source_markdown(_link_visible_sources(seg["text"], srcs or [], meta)).classes(
                    "sov-chat-message-text sov-chat-md"
                )
    return True

# ── ФАЙЛЫ-АРТЕФАКТЫ: готовые документы (смета xlsx, формы) в панели «Файлы» ──
