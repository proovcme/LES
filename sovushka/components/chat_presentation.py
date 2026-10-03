"""Pure presentation of chat metadata, timings, attachments and status messages."""
from __future__ import annotations
import re
from datetime import datetime
from pathlib import Path
from typing import Any

def format_chat_duration_sec(value: float | int | None) -> str:
    """Human-readable duration for the answer timing line."""

    if value is None:
        return ""
    try:
        seconds = max(0.0, float(value))
    except (TypeError, ValueError):
        return ""
    if seconds >= 3600:
        hours = int(seconds // 3600)
        minutes = int((seconds % 3600) // 60)
        return f"{hours}ч {minutes}м"
    if seconds >= 60:
        minutes, remainder = divmod(int(round(seconds)), 60)
        return f"{minutes}м {remainder}с"
    if seconds >= 10:
        return f"{int(round(seconds))}с"
    text = f"{seconds:.1f}".rstrip("0").rstrip(".")
    return f"{text}с"

def workbook_chat_filename(item: Any) -> str:
    """Safe workbook label for chat history and download controls."""

    payload = item if isinstance(item, dict) else {}
    name = str(payload.get("filename") or "").strip().replace("\\", "/")
    if (
        name
        and Path(name).name == name
        and ".." not in name
        and name.lower().endswith(".xlsx")
        and name.lower() not in {".xlsx", "artifact.xlsx"}
    ):
        return name
    return "VOR.xlsx" if payload.get("artifact_kind") == "vor_workbook" else "LSR.xlsx"

def artifact_workbook_files(artifact: Any) -> list[dict[str, str]]:
    """Return every distinct workbook download advertised by an artifact."""

    payload = artifact if isinstance(artifact, dict) else {}
    candidates = payload.get("files") if isinstance(payload.get("files"), list) else []
    direct_url = str(payload.get("download_url") or "").strip()
    if direct_url:
        candidates = [
            *candidates,
            {
                "download_url": direct_url,
                "filename": payload.get("filename"),
                "artifact_kind": payload.get("artifact_kind"),
            },
        ]
    files: list[dict[str, str]] = []
    seen_urls: set[str] = set()
    for item in candidates:
        if not isinstance(item, dict):
            continue
        url = str(item.get("download_url") or "").strip()
        if not url or url in seen_urls:
            continue
        seen_urls.add(url)
        kind = str(item.get("artifact_kind") or "").strip()
        if kind not in {"lsr_workbook", "vor_workbook"}:
            kind = "vor_workbook" if workbook_chat_filename(item).startswith("VOR") else "lsr_workbook"
        files.append({
            "download_url": url,
            "filename": workbook_chat_filename({**item, "artifact_kind": kind}),
            "artifact_kind": kind,
        })
    return files

def format_chat_request_clock(requested_at: Any) -> str:
    """Local date and clock for the user request."""

    if requested_at in (None, ""):
        return ""
    dt: datetime | None = None
    if isinstance(requested_at, (int, float)):
        try:
            dt = datetime.fromtimestamp(float(requested_at)).astimezone()
        except (OverflowError, OSError, ValueError):
            return ""
    else:
        raw = str(requested_at).strip()
        if not raw:
            return ""
        try:
            dt = datetime.fromisoformat(raw.replace("Z", "+00:00"))
            dt = dt.astimezone()
        except ValueError:
            return ""
    return dt.strftime("%d.%m.%Y %H:%M") if dt is not None else ""

def resolve_answer_timing(
    *,
    requested_at: Any = None,
    elapsed_sec: float | int | None = None,
    latency_phases: dict | None = None,
    model_think_sec: float | int | None = None,
) -> dict[str, Any]:
    """Prefer authoritative backend phase timings over client estimates."""

    phases = latency_phases if isinstance(latency_phases, dict) else {}
    elapsed = phases.get("wall_total", elapsed_sec)
    think = phases.get("generation", model_think_sec)
    try:
        elapsed = float(elapsed) if elapsed is not None else None
    except (TypeError, ValueError):
        elapsed = None
    try:
        think = float(think) if think is not None else None
    except (TypeError, ValueError):
        think = None
    return {
        "requested_at": requested_at,
        "elapsed_sec": elapsed,
        "model_think_sec": think,
    }

def format_answer_timing_line(
    *,
    requested_at: Any = None,
    elapsed_sec: float | int | None = None,
    model_think_sec: float | int | None = None,
    latency_phases: dict | None = None,
) -> str:
    """Compact request date plus total/model durations."""

    timing = resolve_answer_timing(
        requested_at=requested_at,
        elapsed_sec=elapsed_sec,
        latency_phases=latency_phases,
        model_think_sec=model_think_sec,
    )
    parts: list[str] = []
    clock = format_chat_request_clock(timing["requested_at"])
    if clock:
        parts.append(clock)
    elapsed_label = format_chat_duration_sec(timing["elapsed_sec"])
    if elapsed_label:
        parts.append(f"ответ {elapsed_label}")
    think_label = format_chat_duration_sec(timing["model_think_sec"])
    if think_label:
        parts.append(f"модель {think_label}")
    return " · ".join(parts)

def _operator_status_chips(crag: str, meta: dict | None, srcs: list | None = None) -> list[dict[str, str]]:
    """Human-facing chips for the answer footer.

    The first UI layer should not narrate internal routing/contracts. Evidence
    status, workflow and contract details stay in the technical disclosure.
    """
    chips: list[dict[str, str]] = []
    src_count = len(srcs or [])
    if src_count:
        chips.append({"label": f"{src_count} источн.", "tone": "ok"})
    latency = (meta or {}).get("latency_phases")
    raw_total = latency.get("total") if isinstance(latency, dict) else None
    try:
        total_seconds = float(raw_total)
    except (TypeError, ValueError):
        total_seconds = -1.0
    if 0 <= total_seconds < 60:
        elapsed = f"{total_seconds:.1f}".replace(".", ",")
        chips.append({"label": f"{elapsed} с", "tone": "muted"})
    elif total_seconds >= 60:
        rounded_seconds = int(round(total_seconds))
        minutes, seconds = divmod(rounded_seconds, 60)
        chips.append({"label": f"{minutes} мин {seconds} с", "tone": "muted"})
    return chips

def _operator_technical_chips(meta: dict | None) -> list[str]:
    """Compact internal trace chips, hidden behind a details expander."""
    if not meta:
        return []
    out: list[str] = []
    query_route = meta.get("query_route") if isinstance(meta.get("query_route"), dict) else {}
    kot = query_route.get("kot") if isinstance(query_route.get("kot"), dict) else {}
    trace = meta.get("retrieval_trace") if isinstance(meta.get("retrieval_trace"), dict) else {}
    validation = meta.get("validation") if isinstance(meta.get("validation"), dict) else {}
    scenario = meta.get("scenario") if isinstance(meta.get("scenario"), dict) else {}
    contract = meta.get("answer_contract") if isinstance(meta.get("answer_contract"), dict) else {}
    contract_check = meta.get("answer_contract_check") if isinstance(meta.get("answer_contract_check"), dict) else {}
    workflow = meta.get("workflow_plan") if isinstance(meta.get("workflow_plan"), dict) else {}
    if kot:
        out.append(f"KOT {kot.get('dataset_filter') or 'AUTO'} {kot.get('confidence', 0)}")
    if trace:
        mode = str(trace.get("mode") or "vector").upper()
        quality = trace.get("quality_status") or trace.get("quality", {}).get("status") or "?"
        out.append(f"{mode} {quality}")
        context_window = trace.get("context_window") if isinstance(trace.get("context_window"), dict) else {}
        if context_window:
            out.append(f"CTX {context_window.get('expanded_count', 0)}/{context_window.get('input_count', 0)}")
    cache_state = str(meta.get("cache") or "miss").strip().lower()
    if cache_state in {"hit", "semantic", "session", "exact"}:
        out.append("Кэш: использован сохранённый ответ")
    elif cache_state == "miss":
        out.append("Кэш: не использован — ответ сформирован заново")
    elif cache_state == "stream_recovered":
        out.append("Ответ восстановлен после обрыва соединения")
    if validation:
        out.append("VALIDATOR ON" if validation.get("enabled") else "VALIDATOR OFF")
    if scenario.get("id"):
        out.append(f"SCENARIO {scenario.get('id')}")
    if contract.get("id"):
        out.append(f"CONTRACT {contract.get('id')}")
    if contract_check.get("status"):
        out.append(f"CONTRACT_CHECK {str(contract_check.get('status')).upper()}")
        missing = contract_check.get("missing")
        if isinstance(missing, list) and missing:
            out.append(f"MISSING {','.join(str(x) for x in missing[:4])}")
    if workflow.get("workflow_id"):
        out.append(f"WORKFLOW {workflow.get('workflow_id')}")
    if workflow.get("status"):
        out.append(f"WF_STATUS {workflow.get('status')}")
    if workflow.get("finality"):
        out.append(f"WF_FINALITY {workflow.get('finality')}")
    wf_missing = workflow.get("missing_inputs")
    if isinstance(wf_missing, list) and wf_missing:
        out.append(f"WF_MISSING {','.join(str(x) for x in wf_missing[:4])}")
    wf_actions = workflow.get("next_actions")
    if isinstance(wf_actions, list) and wf_actions:
        out.append(f"WF_ACTION {str(wf_actions[0])[:80]}")
    return out

def _dataset_profile_operator_summary(profile: dict) -> list[str]:
    """Short operator summary of a dataset passport."""
    if not isinstance(profile, dict):
        return []
    lines = [
        f"{profile.get('name') or profile.get('dataset_id')}: "
        f"{profile.get('document_count', 0)} файлов, {profile.get('chunk_count', 0)} чанков",
    ]
    deep = profile.get("deep") if isinstance(profile.get("deep"), dict) else {}
    norm_refs = ", ".join((deep.get("norm_refs") or [])[:5])
    keywords = ", ".join((deep.get("content_keywords") or profile.get("keywords") or [])[:8])
    if norm_refs:
        lines.append(f"Нормативы/ссылки: {norm_refs}")
    if keywords:
        lines.append(f"Темы: {keywords}")
    if deep.get("table_signal_chunks"):
        lines.append(f"Табличный сигнал: {deep.get('table_signal_chunks')} фрагм.")
    if profile.get("profile_path"):
        lines.append(f"Файл паспорта: {profile.get('profile_path')}")
    return lines

def _dataset_notebook_operator_summary(notebook: dict) -> list[str]:
    if not isinstance(notebook, dict):
        return []
    profile = notebook.get("profile") if isinstance(notebook.get("profile"), dict) else notebook
    lines = _dataset_profile_operator_summary(profile)
    summary = notebook.get("notebook_summary") if isinstance(notebook.get("notebook_summary"), dict) else {}
    areas = ", ".join((summary.get("subject_areas") or [])[:6])
    terms = ", ".join((summary.get("key_terms") or [])[:8])
    if areas:
        lines.append(f"Области: {areas}")
    if terms and not any(line.startswith("Темы:") for line in lines):
        lines.append(f"Темы: {terms}")
    lines.append("Роль: навигация, не evidence.")
    return lines

def _chat_profile_operator_summary(profile: dict) -> list[str]:
    if not isinstance(profile, dict) or not profile:
        return []
    lines = [
        f"Ходов: {profile.get('turn_count', 0)} · последний статус: {profile.get('last_status') or 'unknown'}",
    ]
    if profile.get("effective_dataset_filter"):
        lines.append(f"Текущий фильтр: {profile.get('effective_dataset_filter')}")
    if profile.get("blockers"):
        lines.append("Не хватает: " + "; ".join(profile.get("blockers", [])[-3:]))
    if profile.get("assumptions"):
        lines.append("Допущения: " + "; ".join(profile.get("assumptions", [])[-3:]))
    return lines

def _attachment_chat_payload(attachment: dict) -> dict:
    """Скрепка → поля ChatRequest. Чистая функция для тестов и чтобы UI не забывал scope."""
    if not attachment or not attachment.get("id"):
        return {}
    mode = attachment.get("mode")
    payload: dict = {}
    if mode in {"quick", "index"}:
        payload["dataset_ids"] = [str(attachment["id"])]
    if mode == "read":
        attachment = {**attachment, "text": attachment.get("text") or ""}
        payload["attachment_id"] = str(attachment["id"])
        name = str(attachment.get("name") or "вложение").strip() or "вложение"
        payload["attachment_context"] = f"Файл: {name}\n\n{str(attachment['text']).strip()}"
    return payload

def _preserved_attachment(result: dict | None, sent: dict | None) -> dict:
    """Return the sent attachment only when backend kept it for a retry."""

    retry = (result or {}).get("attachment_retry") or {}
    sent_copy = dict(sent or {})
    retry_id = str(retry.get("attachment_id") or retry.get("id") or "")
    if not retry.get("preserved") or not sent_copy.get("id"):
        return {}
    if retry_id != str(sent_copy.get("id") or ""):
        return {}
    return sent_copy

def _attachment_visible_text(data: dict) -> tuple[str, str, str]:
    """Текст видимого подтверждения: куда именно пойдёт файл после галочки upload."""
    name = str(data.get("name") or "файл").strip() or "файл"
    mode = str(data.get("mode") or "")
    if mode == "read":
        if data.get("media_type") == "image":
            return ("Изображение прикреплено к следующему сообщению",
                    f"{name} · Изображение",
                    f"📎 Изображение «{name}» прикреплено. Для ответа нужна модель с поддержкой изображений.")
        suffix = " · усечён" if data.get("truncated") else ""
        return (
            "Файл прикреплён к следующему сообщению",
            f"{name} · В чат · {data.get('chars', 0)} симв.{suffix}",
            f"📎 Файл «{name}» прикреплён к следующему запросу. Модель увидит его текст вместе с сообщением.",
        )
    if mode == "quick":
        return (
            "Таблица прикреплена к следующему сообщению",
            f"{name} · Таблица · {data.get('rows', 0)} строк",
            f"📎 Таблица «{name}» прикреплена к следующему запросу как временный датасет для сверки.",
        )
    return (
        "Файл добавлен в базу и выбран для следующего сообщения",
        f"{name} · В базу · {data.get('dataset_name', 'RAG-датасет')}",
        f"📎 Файл «{name}» добавлен в базу и будет выбран как источник следующего запроса.",
    )

def _attachment_user_suffix(attachment: dict) -> str:
    """Строка в пользовательском сообщении: вложение должно остаться в истории диалога."""
    if not attachment or not attachment.get("id"):
        return ""
    name = str(attachment.get("name") or "файл").strip() or "файл"
    mode = str(attachment.get("mode") or "")
    if mode == "read":
        chars = int(attachment.get("chars") or len(str(attachment.get("text") or "")) or 0)
        suffix = f" · {chars} симв." if chars else ""
        return f"📎 Прикреплён файл: {name} · В чат{suffix}"
    if mode == "quick":
        rows = int(attachment.get("rows") or 0)
        suffix = f" · {rows} строк" if rows else ""
        return f"📎 Прикреплена таблица: {name} · Таблица{suffix}"
    dataset_name = str(attachment.get("dataset_name") or "RAG-датасет").strip() or "RAG-датасет"
    return f"📎 Файл добавлен в базу: {name} · {dataset_name}"

def _runtime_guard_reason_label(value: object) -> str:
    """Render resource-admission diagnostics without raw environment keys."""
    raw = str(value or "").strip()
    match = re.fullmatch(r"ram_free_gb=([0-9.]+)\s*<\s*([0-9.]+)", raw)
    if match:
        free, threshold = (part.replace(".", ",") for part in match.groups())
        return f"свободная память {free} ГБ ниже порога {threshold} ГБ"
    match = re.fullmatch(r"swap_pct=([0-9.]+)\s*>\s*([0-9.]+)(?:\s*\(swap_used_gb=([0-9.]+)\))?", raw)
    if match:
        used, threshold, used_gb = match.groups()
        detail = f"использование файла подкачки {used.replace('.', ',')}% выше порога {threshold.replace('.', ',')}%"
        if used_gb:
            detail += f"; занято {used_gb.replace('.', ',')} ГБ"
        return detail
    match = re.fullmatch(r"active_jobs=(\d+)", raw)
    if match:
        return f"выполняются фоновые задачи: {match.group(1)}"
    if raw == "llm_generation_slots=0":
        return "все слоты модели заняты"
    return raw if raw and not re.search(r"[A-Za-z_]", raw) else "недостаточно свободных ресурсов для нового запроса"
