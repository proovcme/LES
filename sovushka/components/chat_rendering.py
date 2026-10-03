"""Stateless answer rendering, evidence labels and artifact format readers."""
from __future__ import annotations
import logging

logger = logging.getLogger(__name__)
from sovushka.components.chat_presentation import format_answer_timing_line
import json
import re
from typing import Optional
from nicegui import ui
from sovushka.components.charts import _html, esc
from sovushka.safe_markup import sanitize_svg
from sovushka.state import api_get_bytes, last_api_error_text

def _copy_js(text: str) -> str:
    """Client-side JS: копировать В ЖЕСТЕ клика (без серверного round-trip — иначе жест теряется и
    execCommand/clipboard блокируются браузером; за http-туннелем navigator.clipboard недоступен).
    navigator.clipboard на secure-context, иначе execCommand-fallback; короткий клиентский тост."""
    t = json.dumps(text or "")
    return (
        "(e) => { const t = " + t + ";"
        " const toast = (ok) => { try { const d = document.createElement('div');"
        "  d.textContent = ok ? 'Скопировано' : 'Не вышло — выдели и Ctrl+C';"
        "  d.style.cssText = 'position:fixed;bottom:24px;left:50%;transform:translateX(-50%);"
        "background:#222;color:#fff;padding:8px 14px;border-radius:8px;z-index:99999;font:13px sans-serif';"
        "  document.body.appendChild(d); setTimeout(() => d.remove(), 1500); } catch (_) {} };"
        " const fb = () => { try { const ta = document.createElement('textarea'); ta.value = t;"
        "  ta.style.position='fixed'; ta.style.top='0'; ta.style.opacity='0';"
        "  document.body.appendChild(ta); ta.focus(); ta.select();"
        "  const ok = document.execCommand('copy'); document.body.removeChild(ta); toast(ok); }"
        "  catch (_) { toast(false); } };"
        " try { if (navigator.clipboard && window.isSecureContext) {"
        "   navigator.clipboard.writeText(t).then(() => toast(true)).catch(fb); } else { fb(); } }"
        " catch (_) { fb(); } }"
    )


def _copy_button(label: str, text: str, *, icon: str = "o_content_copy",
                 props: str = "flat dense no-caps", classes: str = ""):
    """Кнопка «Копировать» с КЛИЕНТСКИМ обработчиком (копирует в жесте → работает и по http/туннелю,
    не только на localhost/https). Текст известен на рендере — зашит в js_handler."""
    btn = ui.button(label, icon=icon).props(props)
    if classes:
        btn.classes(classes)
    btn.on("click", js_handler=_copy_js(text))
    return btn


def _rows_to_csv(rows: list[dict]) -> bytes:
    """list[dict] → CSV для рус-Excel: разделитель «;», UTF-8 BOM (кириллица не ломается)."""
    import csv
    import io
    if not rows:
        return "﻿".encode("utf-8")
    keys: list[str] = []
    for r in rows:
        if isinstance(r, dict):
            for k in r:
                if k not in keys:
                    keys.append(k)
    buf = io.StringIO()
    w = csv.DictWriter(buf, fieldnames=keys, extrasaction="ignore", delimiter=";")
    w.writeheader()
    for r in rows:
        if isinstance(r, dict):
            w.writerow({k: ("" if r.get(k) is None else r.get(k)) for k in keys})
    return ("﻿" + buf.getvalue()).encode("utf-8")


def _source_label(source) -> str:
    # v0.16: «N · file · абз.85» вместо сырого пути; нет ref → «без ссылки» (не фейк-линк).
    from sovushka.answer_render import source_chip
    c = source_chip(source)
    if not c["has_ref"]:
        if isinstance(source, dict):
            return str(source.get("doc_name") or source.get("file") or source.get("name") or "без ссылки")
        return str(source) or "без ссылки"
    parts = [c["file"]] + ([c["locator"]] if c["locator"] else [])
    return " · ".join(p for p in parts if p)


def _render_excerpts(meta: dict | None):
    """Конкретные фрагменты норм/документов, на которые опёрся ответ — «вот это
    место». Раскрываемо; ссылка «открыть» ведёт в файл (W18.1) если есть путь."""
    if not meta:
        return
    excerpts = meta.get("source_excerpts") or []
    if not excerpts:
        return
    from urllib.parse import quote
    with ui.expansion(f"Цитаты из источников ({len(excerpts)})", icon="format_quote").props(
        "dense"
    ).classes("w-full mt-2").style("font-size:.66rem;"):
        for ex in excerpts:
            doc = ex.get("doc", "") or ""
            with ui.column().classes("w-full gap-1").style(
                "border-left:2px solid var(--accent);padding:3px 0 8px 10px;margin-top:6px;"
            ):
                with ui.row().classes("w-full items-center gap-2"):
                    ui.label(doc.rsplit("/", 1)[-1] or "источник").style(
                        "font-size:.64rem;color:var(--accent);font-weight:700;word-break:break-all;"
                    )
                    if ex.get("score") is not None:
                        ui.label(f"score {ex['score']}").style("font-size:.56rem;color:var(--dim);")
                    if "/" in doc:
                        ui.link("открыть", f"/lite-api/rag/file/raw?path={quote(doc)}").props(
                            "target=_blank"
                        ).style("font-size:.58rem;color:var(--ok);margin-left:auto;")
                ui.label(ex.get("text", "")).style(
                    "font-size:.7rem;line-height:1.5;color:var(--text);white-space:pre-wrap;"
                )


def _artifact_present(ans: str, mode: str) -> bool:
    """Есть ли в ответе рендеримый артефакт (таблица/спека/диаграмма/svg)."""
    ans = ans or ""
    if mode in ("spec", "table", "structure", "template", "schema"):
        return bool(_parse_table_from_ai(ans) or _parse_json_from_ai(ans))
    if mode == "mermaid":
        return bool(_parse_mermaid_from_ai(ans))
    if mode == "svg":
        return bool(_parse_svg_from_ai(ans))
    # text/дефолт: авто-детект таблицы или диаграммы в обычном ответе
    return bool(_parse_table_from_ai(ans) or _parse_markdown_table(ans)
                or _parse_mermaid_from_ai(ans) or _parse_svg_from_ai(ans))


def _inventory_file_rows_from_meta(meta: dict | None) -> list[dict]:
    """Structured project inventory → flat UI rows."""
    if not isinstance(meta, dict):
        return []
    artifact = meta.get("artifact") if isinstance(meta.get("artifact"), dict) else {}
    candidates = [
        artifact,
        artifact.get("project_inventory") if isinstance(artifact, dict) else None,
        meta.get("project_inventory"),
    ]
    seen: set[str] = set()
    rows: list[dict] = []

    def _add_row(item, folder: str = "") -> None:
        if isinstance(item, dict):
            name = str(
                item.get("file_name")
                or item.get("path")
                or item.get("source_path")
                or item.get("name")
                or ""
            ).strip()
            status = str(item.get("status") or item.get("index_status") or item.get("state") or "").strip()
            chunk_count = item.get("chunk_count")
            content_layers = item.get("content_layers") or []
            content_layer_labels = item.get("content_layer_labels") or []
            file_kind = str(item.get("file_kind") or "").strip()
            document_role = str(item.get("document_role") or "").strip()
            source_granularity = str(item.get("source_granularity") or "").strip()
        elif isinstance(item, (list, tuple)) and item:
            name = str(item[0] or "").strip()
            status = str(item[1] if len(item) > 1 else "").strip()
            chunk_count = item[2] if len(item) > 2 else None
            content_layers = []
            content_layer_labels = []
            file_kind = ""
            document_role = ""
            source_granularity = ""
        else:
            name = str(item or "").strip()
            status = ""
            chunk_count = None
            content_layers = []
            content_layer_labels = []
            file_kind = ""
            document_role = ""
            source_granularity = ""
        if not name:
            return
        target = name
        if folder and "/" not in name and folder != "(корень)":
            target = f"{folder.rstrip('/')}/{name}"
        if target in seen:
            return
        seen.add(target)
        try:
            chunks = int(chunk_count) if chunk_count not in (None, "") else None
        except Exception:
            chunks = None
        rows.append({
            "file_name": target,
            "display_name": name.rsplit("/", 1)[-1],
            "folder": folder,
            "status": (status or "UNKNOWN").upper(),
            "chunk_count": chunks,
            "content_layers": [str(x) for x in content_layers if str(x).strip()],
            "content_layer_labels": [str(x) for x in content_layer_labels if str(x).strip()],
            "file_kind": file_kind,
            "document_role": document_role,
            "source_granularity": source_granularity,
        })

    def _walk(payload) -> None:
        if not isinstance(payload, dict):
            return
        files = payload.get("files")
        if isinstance(files, list):
            for item in files:
                _add_row(item)
        inventory = payload.get("inventory") if isinstance(payload.get("inventory"), dict) else payload
        folders = inventory.get("folders") if isinstance(inventory, dict) else {}
        if isinstance(folders, dict):
            for folder, items in folders.items():
                if isinstance(items, list):
                    for item in items:
                        _add_row(item, str(folder or ""))

    for candidate in candidates:
        _walk(candidate)
    return rows


def _artifact_from_meta(meta: dict | None) -> dict:
    artifact = (meta or {}).get("artifact") or {}
    if not isinstance(artifact, dict):
        return {}
    if str(artifact.get("content") or "").strip():
        return artifact
    return artifact if _inventory_file_rows_from_meta(meta) else {}


def _bubble_text(ans: str, mode: str) -> str:
    """Текст для пузыря: если есть артефакт — убираем дублирующую таблицу/код-блоки
    (они уже в артефакте), оставляем только коммент модели. Олег: «текст-то зачем?»."""
    ans = ans or ""
    if not _artifact_present(ans, mode):
        return ans
    t = re.sub(r"```.*?```", "", ans, flags=re.DOTALL)           # fenced json/svg/mermaid
    t = "\n".join(ln for ln in t.splitlines() if not ln.strip().startswith("|"))  # md-таблица
    t = re.sub(r"\n{3,}", "\n\n", t).strip()
    return t or "Готово — результат в артефакте (кнопка ниже)."


async def _download_file_artifact(url: str, name: str) -> None:
    res = await api_get_bytes(url)
    if not res:
        ui.notify(last_api_error_text("Файл не готов"), type="negative")
        return
    data, fname = res
    ui.download(data, name or fname)


def _rows_from_spreadsheet(data: bytes, kind: str) -> list[dict]:
    """xlsx/csv байты → list[dict] (первый лист, шапка = первая строка). Лимит 200 строк."""
    try:
        if kind == "csv":
            import csv
            import io
            text = data.decode("utf-8-sig", errors="replace")
            reader = csv.reader(io.StringIO(text), delimiter=";" if ";" in text.split("\n", 1)[0] else ",")
            table = list(reader)
        else:
            import io
            import openpyxl
            wb = openpyxl.load_workbook(io.BytesIO(data), read_only=True, data_only=True)
            ws = wb.active
            table = [[("" if c is None else str(c)) for c in row]
                     for row in ws.iter_rows(values_only=True)]
        if not table:
            return []
        headers = [str(h or f"col{i+1}") for i, h in enumerate(table[0])]
        out = []
        for row in table[1:201]:
            vals = (list(row) + [""] * len(headers))[: len(headers)]
            out.append({headers[i]: vals[i] for i in range(len(headers))})
        return out
    except Exception:
        return []


def _kind_from_name(name: str) -> str:
    ext = name.rsplit(".", 1)[-1].lower() if "." in name else ""
    if ext in ("png", "jpg", "jpeg", "gif", "webp"):
        return "image"
    if ext in ("xlsx", "xls", "csv", "docx", "pdf"):
        return "xlsx" if ext in ("xlsx", "xls") else ext
    return "file"


def _render_evidence_header(meta: dict | None, srcs: list | None) -> None:
    """v0.16: компактная статус-полоска ответа — статус + бейджи evidence (RETRIEVED/COMPUTED/
    ASSUMED/MISSING/BLOCKED) + источники + intent + свёрнутый trace. Нет evidence → ничего
    (старый рендер сохраняется)."""
    if not meta:
        return
    from sovushka.answer_render import (
        header_summary,
        retrieval_notice,
        tool_execution_notice,
        source_count_labels,
        trace_summary,
    )
    from sovushka.uikit import render_feedback_state, status_badge
    if (meta.get("retrieval_trace") or {}).get("stream_recovery"):
        ui.label("Неполный ответ: соединение оборвалось. Сохранён полученный фрагмент.").props(
            'role="status" aria-live="polite"'
        ).classes("sov-ui-section-detail")
    tool_notice = tool_execution_notice(meta.get("retrieval_trace"))
    if tool_notice:
        render_feedback_state("error", detail=tool_notice)
    h = header_summary(meta.get("query_route"), meta.get("evidence_summary"),
                       len(srcs or []), meta.get("total_status"))
    if not h["has_evidence"]:
        return
    with ui.row().classes("sov-ev-header"):
        st = h["status"]
        ui_tone = {"ok": "ok", "warn": "warn", "err": "error"}.get(
            st["tone"], "muted"
        )
        status_badge(st["label"], ui_tone).classes(
            f"sov-ev-status sov-ev-{st['tone']}"
        )
        for b in h["badges"]:
            ui.label(f"{b['label']} {b['count']}").classes(f"sov-ev-badge sov-ev-{b['tone']}")
        if h["sources_count"]:
            ui.label(f"{h['sources_count']} ист.").classes("sov-ev-meta")
        source_counts = meta.get("source_counts") or (
            meta.get("retrieval_trace") or {}
        ).get("source_counts")
        if isinstance(source_counts, dict):
            for count_label in source_count_labels(source_counts):
                ui.label(count_label).classes("sov-ev-meta")
    ts = trace_summary(meta.get("unified_trace"))
    if ts:
        with ui.expansion("Технические подробности").classes("sov-ev-trace"):
            ui.label(ts).classes("sov-ev-trace-text")
    retrieval_trace = (
        meta.get("retrieval_trace")
        if isinstance(meta.get("retrieval_trace"), dict)
        else {}
    )
    blocker = meta.get("blocker") if isinstance(meta.get("blocker"), dict) else {}
    notice = retrieval_notice(retrieval_trace)
    if notice and notice.get("status") == "degraded":
        with ui.element("div").classes("sov-retrieval-notice sov-retrieval-notice--warn"):
            ui.label(notice["title"]).classes("sov-retrieval-notice-title")
            ui.label(notice["detail"]).classes("sov-retrieval-notice-detail")
    if retrieval_trace.get("status") == "blocked" or meta.get("total_status") == "blocked":
        render_feedback_state(
            "blocked",
            error_code=str(
                blocker.get("code") or retrieval_trace.get("error_code") or ""
            ),
            detail=str(blocker.get("action") or ""),
        )


def _model_label(provider: str = "", model: str = "") -> str:
    provider = str(provider or "").strip()
    model = str(model or "").strip()
    if not provider and not model:
        return ""
    if model:
        short = model.rsplit("/", 1)[-1]
        if len(short) > 34:
            short = short[:31].rstrip() + "..."
        return f"{provider or 'model'} · {short}"
    return provider


def _answer_model_label(meta: dict | None) -> str:
    """Human label for the model that produced this answer."""
    if not isinstance(meta, dict):
        return ""
    connection = meta.get("model_connection") if isinstance(meta.get("model_connection"), dict) else {}
    if connection:
        return _model_label(
            str(connection.get("display_name") or "подключение"),
            str(connection.get("model_id") or ""),
        )
    trace = meta.get("retrieval_trace") if isinstance(meta.get("retrieval_trace"), dict) else {}
    routing = trace.get("routing") if isinstance(trace.get("routing"), dict) else {}
    versions = meta.get("versions") if isinstance(meta.get("versions"), dict) else {}
    provider = routing.get("effective_provider") or versions.get("llm_provider") or ""
    model = routing.get("effective_model") or versions.get("llm_model") or ""
    return _model_label(provider, model)


def _render_model_badge(meta: dict | None) -> None:
    label = _answer_model_label(meta)
    if label:
        ui.label(f"МОДЕЛЬ {label}").classes("sov-model-badge").tooltip(
            "Модель/провайдер этого ответа"
        )


def _render_dataset_scope_badge(meta: dict | None) -> None:
    if not isinstance(meta, dict):
        return
    trace = meta.get("retrieval_trace") if isinstance(meta.get("retrieval_trace"), dict) else {}
    scope = meta.get("source_scope") if isinstance(meta.get("source_scope"), dict) else trace.get("source_scope")
    if not isinstance(scope, dict):
        return
    names = [str(item) for item in (scope.get("used_names") or []) if str(item)]
    if not names:
        return
    label = ", ".join(names[:2]) + (f" +{len(names) - 2}" if len(names) > 2 else "")
    ui.label(f"ДАТАСЕТЫ {label}").classes("sov-model-badge").tooltip(
        "Фактические датасеты, из которых взяты фрагменты ответа"
    )


def _render_answer_timing(meta: dict | None) -> None:
    payload = meta if isinstance(meta, dict) else {}
    line = format_answer_timing_line(
        requested_at=payload.get("requested_at"),
        elapsed_sec=payload.get("elapsed_sec"),
        model_think_sec=payload.get("model_think_sec"),
        latency_phases=payload.get("latency_phases"),
    )
    if line:
        ui.label(line).classes("sov-chat-timing").tooltip(
            "Время запроса, полная длительность и время работы модели"
        )


def _render_answer_actions(text: str, srcs: list) -> None:
    """v0.20: панель действий ответа — «Копировать» (чистый текст) и «С источниками» (без полного
    тела письма — только chip-локатор). Без скрытого trace."""
    from sovushka.answer_render import answer_copy_text
    with ui.row().classes("sov-answer-actions").style("gap:4px;margin-top:6px;"):
        # Клиентское копирование в жесте клика — работает и по http/туннелю (не только localhost).
        _copy_button("Копировать", answer_copy_text(text), classes="sov-answer-act")
        if srcs:
            _copy_button("С источниками", answer_copy_text(text, srcs, with_sources=True),
                         icon="o_format_quote", classes="sov-answer-act")


def _render_ai_placeholder(text: str):
    with ui.element("div").classes("chat-msg-ai typing") as bubble:
        label = ui.label(text).classes("sov-chat-message-text")
    return bubble, label


def _render_empty_artifacts():
    _html(
        '<div class="sov-artifact-empty">'
        '<div class="sov-artifact-empty-title">Пока пусто</div>'
        '<div class="sov-muted">Структурированные ответы, таблицы, SVG и диаграммы появятся здесь.</div>'
        '</div>'
    )


def _parse_table_from_ai(text: str):
    match = re.search(r"```(?:json)?\s*(\[.*?\])\s*```", text, re.DOTALL)
    if match:
        try:
            return json.loads(match.group(1))
        except Exception:
            pass
    return None


def _parse_markdown_table(text: str):
    """Первую markdown-таблицу (| a | b | + строка-разделитель) → list[dict]. Иначе None."""
    lines = [ln.rstrip() for ln in (text or "").splitlines()]
    block: list[str] = []
    for ln in lines:
        s = ln.strip()
        if s.startswith("|") and s.count("|") >= 2:
            block.append(s)
        elif block:
            break  # таблица закончилась — берём первую целостную
    if len(block) < 2:
        return None

    def _cells(row: str) -> list[str]:
        return [c.strip() for c in row.strip().strip("|").split("|")]

    # вторая строка должна быть разделителем (---|:--: и т.п.)
    if not re.match(r"^\s*\|?[\s:|-]+\|?\s*$", block[1]):
        return None
    headers = _cells(block[0])
    rows: list[dict] = []
    for row in block[2:]:
        vals = _cells(row)
        if not any(vals):
            continue
        vals = (vals + [""] * len(headers))[: len(headers)]
        rows.append({h or f"col{i+1}": vals[i] for i, h in enumerate(headers)})
    return rows or None


def _parse_json_from_ai(text: str):
    match = re.search(r"```(?:json)?\s*(\{.*?\}|\[.*?\])\s*```", text, re.DOTALL)
    if match:
        try:
            return json.loads(match.group(1))
        except Exception:
            pass
    try:
        start = text.find("{") if "{" in text else text.find("[")
        if start >= 0:
            end = text.rfind("}") if "{" in text else text.rfind("]")
            return json.loads(text[start : end + 1])
    except Exception:
        pass
    return None


def _parse_mermaid_from_ai(text: str) -> Optional[str]:
    match = re.search(r"```mermaid\s*(.*?)```", text, re.DOTALL)
    return match.group(1).strip() if match else None


def _parse_svg_from_ai(text: str) -> Optional[str]:
    match = re.search(r"```svg\s*(.*?)```", text, re.DOTALL)
    if match:
        return sanitize_svg(match.group(1).strip())
    match = re.search(r"(<svg[\s\S]*?</svg>)", text, re.IGNORECASE)
    return sanitize_svg(match.group(1).strip()) if match else None


def _render_table(data: list[dict]):
    keys = list(data[0].keys()) if data else []
    cols = [{"name": k, "label": k, "field": k, "align": "left", "sortable": True} for k in keys]
    with ui.element("div").classes("sov-table-scroll"):
        ui.table(columns=cols, rows=data, pagination={"rowsPerPage": 0}).classes("sov-artifact-table")
    with ui.row().classes("gap-2"):
        ui.button(
            "CSV",
            icon="o_download",
            on_click=lambda d=data: ui.download(_rows_to_csv(d), "specification.csv"),
        ).props("no-caps flat dense").tooltip("Скачать CSV (рус-Excel: ; + UTF-8 BOM)")
        _copy_button("JSON", json.dumps(data, ensure_ascii=False, indent=2), props="no-caps flat dense")


def _render_gost_spec(data: list[dict]):
    """Рендер спецификации по форме ГОСТ 21.110-2013: графы с правильными
    заголовками + рамка + заголовок формы + CSV/JSON с ГОСТ-шапкой."""
    gost = [
        ("поз", "Поз."),
        ("обозначение", "Обозначение"),
        ("наименование", "Наименование и техническая характеристика"),
        ("тип_марка", "Тип, марка, обозначение документа / опросного листа"),
        ("код", "Код продукции / завод-изготовитель"),
        ("ед_изм", "Ед. изм."),
        ("кол_во", "Кол."),
        ("масса_ед", "Масса ед., кг"),
        ("примечание", "Примечание"),
    ]
    rows = [r for r in data if isinstance(r, dict)]
    present = [(k, h) for k, h in gost if any(k in r for r in rows)]
    if not present:  # модель отдала иные ключи — показываем как есть
        present = [(k, k) for k in (rows[0].keys() if rows else [])]
    _html('<div style="font-size:.72rem;font-weight:900;color:var(--text);'
          'text-transform:uppercase;letter-spacing:.04em;margin-bottom:6px;">'
          'Спецификация оборудования, изделий и материалов · ГОСТ 21.110-2013</div>')
    cols = [{"name": k, "label": h, "field": k, "align": "left"} for k, h in present]
    ui.table(columns=cols, rows=rows, pagination=20, row_key="поз").props(
        "bordered dense flat"
    ).classes("sov-artifact-table")
    # CSV/JSON — с ГОСТ-заголовками граф (рус-Excel читает кириллицу).
    csv_rows = [{h: r.get(k, "") for k, h in present} for r in rows]
    with ui.row().classes("gap-2"):
        ui.button("CSV", icon="o_download",
                  on_click=lambda d=csv_rows: ui.download(_rows_to_csv(d), "specification_gost.csv")
                  ).props("no-caps flat dense").tooltip("Скачать CSV по ГОСТ (; + UTF-8 BOM)")
        _copy_button("JSON", json.dumps(rows, ensure_ascii=False, indent=2), props="no-caps flat dense")


def _render_table_query(table_query: dict):
    try:
        rows = table_query.get("rows") or []
        if not rows:
            ui.markdown("_Нет данных для отображения в таблице_").classes("sov-artifact-markdown")
            return

        # Curate keys: hide internal ones and map to pretty labels
        sample = rows[0]
        visible_keys = [k for k in sample.keys() if not k.startswith("_") and k != "raw_row"]

        pretty_labels = {
            "pos": "№",
            "code": "Код",
            "name": "Наименование работ",
            "work_name": "Наименование работ",
            "unit": "Ед. изм.",
            "qty": "Кол-во",
            "price": "Цена",
            "amount": "Сумма",
            "amount_mat": "Материалы",
            "amount_work": "Работы",
            "work_done": "Выполнено",
            "weight_total": "Масса"
        }

        # Generate column definitions for AG Grid
        column_defs = []
        for k in visible_keys:
            label = pretty_labels.get(k.lower(), k)
            column_defs.append({
                "headerName": label,
                "field": k,
                "filter": True,
                "sortable": True,
                "resizable": True
            })

        aggrid_options = {
            "columnDefs": column_defs,
            "rowData": rows,
            "pagination": True,
            "paginationPageSize": 10,
            "domLayout": "autoHeight"
        }

        # UI elements for table query
        operation = table_query.get("operation") or "list"
        total = table_query.get("total")
        count = table_query.get("count", 0)

        with ui.column().classes("w-full gap-2"):
            # Summary label
            summary_text = f"**Операция:** {operation.upper()}"
            if total is not None:
                summary_text += f" | **Итого:** {total:,.2f}".replace(",", " ")
            summary_text += f" | **Строк:** {count}"
            ui.markdown(summary_text)

            # Render AG Grid
            ui.aggrid(aggrid_options).classes("w-full").style("margin-top: 5px;")

            # Export actions
            with ui.row().classes("gap-2"):
                _copy_button("Копировать JSON", json.dumps(rows, ensure_ascii=False, indent=2), props="no-caps flat dense")

    except Exception as e:
        logger.error(f"Error rendering AG Grid table query: {e}")
        try:
            markdown_lines = []
            sample = rows[0]
            cols = [k for k in sample.keys() if not k.startswith("_") and k != "raw_row"]
            markdown_lines.append("| " + " | ".join(cols) + " |")
            markdown_lines.append("| " + " | ".join(["---"] * len(cols)) + " |")
            for r in rows:
                markdown_lines.append("| " + " | ".join(str(r.get(c) or "") for c in cols) + " |")
            ui.markdown("\n".join(markdown_lines)).classes("sov-artifact-markdown")
        except Exception:
            ui.markdown("_Ошибка отображения таблицы. Данные повреждены._").classes("sov-artifact-markdown")


def _render_tree(data, level: int = 0):
    if isinstance(data, dict):
        name = data.get("name", data.get("title", data.get("id", "—")))
        desc = data.get("desc", data.get("description", ""))
        children = data.get("children", data.get("items", []))
        indent = level * 14
        _html(
            f'<div class="sov-tree-row" style="margin-left:{indent}px;">'
            f'<span class="sov-tree-mark">{"▸" if children else "•"}</span>'
            f'<span class="sov-tree-name">{esc(name)}</span>'
            f'<span class="sov-tree-desc">{esc(desc)}</span>'
            '</div>'
        )
        for child in children if isinstance(children, list) else []:
            _render_tree(child, level + 1)
    elif isinstance(data, list):
        for item in data:
            _render_tree(item, level)
