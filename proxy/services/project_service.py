"""W17.1 — сущность объекта строительства + привязки (проектный режим ЛЕС).

ЛЕС работает в двух режимах: «от объекта» (ретрив/досье сужены к проекту) и
обычный RAG (без фильтра). Здесь — хаб-сущность `les_projects` и лёгкая
тег-таблица `les_project_links` (что относится к объекту). Всё детерминированно,
0 LLM (ADR-11). Таблицы — в метабазе рядом с les_notes/les_tasks/les_field_entries.
"""
from __future__ import annotations

from typing import Any

from backend.rag_config import rag_meta_db_path
from backend.workspace_native import catalog_call

# Доменные виды привязок (что можно отнести к объекту). Расширяется по мере волны.
LINK_KINDS = {"dataset", "cad_bim_import", "task", "field_zahvatka", "mail_thread", "note", "folder"}
PROJECT_STATUSES = {"active", "archived", "on_hold"}


def create_project(name: str, code: str = '', address: str = '') -> dict[str, Any]:
    return catalog_call(rag_meta_db_path(), 'project_create', name=name or '', code=code or '', address=address or '')


def list_projects(limit: int = 100) -> list[dict[str, Any]]:
    return catalog_call(rag_meta_db_path(), 'project_list', limit=limit)


def get_project(project_id: int) -> dict[str, Any] | None:
    return catalog_call(rag_meta_db_path(), 'project_get', project_id=project_id)


def set_project_status(project_id: int, status: str) -> dict[str, Any] | None:
    return catalog_call(rag_meta_db_path(), 'project_status', project_id=project_id, status=status)


def delete_project(project_id: int) -> bool:
    return catalog_call(rag_meta_db_path(), 'project_delete', project_id=project_id)


def link_entity(project_id: int, kind: str, ref: str) -> dict[str, Any]:
    return catalog_call(rag_meta_db_path(), 'project_link', project_id=project_id, kind=kind, ref=ref or '')


def unlink_entity(project_id: int, kind: str, ref: str) -> bool:
    return catalog_call(rag_meta_db_path(), 'project_unlink', project_id=project_id, kind=kind, ref=ref or '')


def list_links(project_id: int, kind: str | None = None) -> list[dict[str, Any]]:
    return catalog_call(rag_meta_db_path(), 'project_links', project_id=project_id, kind=kind)


def project_dataset_ids(project_id: int) -> list[str]:
    """Датасеты, привязанные к объекту — область ретрива в режиме проекта.
    Пусто → проект без своей области (chat не сужает, остаётся обычный RAG)."""
    return [link["ref"] for link in list_links(project_id, kind="dataset")]


def project_for_dataset(dataset_id: str) -> int:
    return catalog_call(rag_meta_db_path(), 'project_for_dataset', dataset_id=str(dataset_id)) if dataset_id else 0


def _datasets_in_scope(dataset_ids: list[str]) -> list[dict[str, Any]]:
    return catalog_call(rag_meta_db_path(), 'project_datasets', dataset_ids=dataset_ids)


def build_registry() -> dict[str, Any]:
    """Реестр проектов (общая карта): все объекты + папки + мета из LES.md. Без LLM."""
    le_meta = {int(key): value for key, value in catalog_call(rag_meta_db_path(), 'project_metadata').items()}

    out = []
    for p in list_projects(limit=500):
        pid = int(p["id"])
        meta = le_meta.get(pid, {})
        folders = [l["ref"] for l in list_links(pid, "folder")]
        out.append({
            "id": pid, "name": p.get("name"), "code": p.get("code") or meta.get("cipher"),
            "address": p.get("address") or meta.get("address"), "stage": meta.get("stage"),
            "status": p.get("status"), "folders": folders,
            "datasets": len(project_dataset_ids(pid)), "has_les_md": pid in le_meta,
        })
    return {"projects": out, "count": len(out)}


def build_dossier(project_id: int) -> dict[str, Any] | None:
    """W17.5 КАРТА ОБЪЕКТА: паспорт объекта одним собранным ответом — нормативы в
    области, решения/задачи, объёмы по захваткам, BIM, связи. Всё детерминированно
    (SQL + существующие сервисы), 0 LLM. PARA-корзины выводятся из статусов."""
    project = get_project(project_id)
    if project is None:
        return None
    by_kind: dict[str, list[str]] = {}
    for link in project.get("links", []):
        by_kind.setdefault(link["kind"], []).append(link["ref"])

    datasets = _datasets_in_scope(by_kind.get("dataset", []))

    # Оперативные данные. Прим.: задачи/объёмы/заметки пока глобальны (партиционирование
    # по объекту — рефайнмент); привязанные явно — в links_by_kind.
    from proxy.services.task_service import list_tasks
    from proxy.services.memory_service import list_notes
    from proxy.services.field_intake_service import aggregate_volumes
    from proxy.services.edge_service import list_edges

    # Q3: оперативные данные партиционированы по объекту — досье строго в рамках объекта.
    all_tasks = list_tasks("", 500, project_id=project_id)
    open_tasks = [t for t in all_tasks if t.get("status") in ("open", "in_progress")]
    closed = sum(1 for t in all_tasks if t.get("status") in ("done", "dropped", "closed"))

    try:
        vol_rows = aggregate_volumes(status="confirmed", project_id=project_id)
    except Exception:
        vol_rows = []
    vol_total = sum(float(r.get("total") or 0) for r in vol_rows)

    notes = list_notes(50, project_id=project_id)
    edges = list_edges(5000)

    # W17.4 — решения проекта (RFI-стиль), строго по объекту (Q3).
    try:
        from proxy.services.decision_service import list_decisions
        decisions = list_decisions(project_id=project_id, limit=50)
    except Exception:
        decisions = []

    bim = None
    try:
        from proxy.services.cad_bim_graph import graph_summary
        bim = graph_summary()
    except Exception:
        bim = None

    # W17.3 — онтология: хребет (своды), состояния CDE контейнеров, LBS-захватки.
    classification = None
    cde = None
    lbs: list[dict[str, Any]] = []
    try:
        from proxy.services.ontology_service import classification_backbone, cde_summary, lbs_hubs
        backbone = classification_backbone()
        classification = {
            "totals": backbone.get("totals", {}),
            "top_floors": [
                {"floor": f["floor"], "elements": f["elements"],
                 "top_systems": [s["system"] for s in f.get("systems", [])[:5]]}
                for f in backbone.get("floors", [])[:6]
            ],
        }
        cde = cde_summary(project_id)
        lbs = lbs_hubs(project_id=project_id)
    except Exception:
        pass

    return {
        "project": {k: project.get(k) for k in ("id", "name", "code", "address", "status", "created_at")},
        "datasets_in_scope": datasets,
        "links_by_kind": {k: len(v) for k, v in by_kind.items()},
        "open_tasks": [
            {"id": t.get("id"), "title": t.get("title"), "status": t.get("status")} for t in open_tasks[:50]
        ],
        "volumes": {
            "groups": len(vol_rows),
            "total": round(vol_total, 2),
            "by_position": [
                {"position": r.get("position"), "unit": r.get("unit"), "total": r.get("total")}
                for r in vol_rows[:20]
            ],
        },
        "notes_count": len(notes),
        "edges_count": len(edges),
        "decisions": [  # W17.4 — решения по объекту (последние)
            {"id": d.get("id"), "decision": d.get("decision"), "status": d.get("status"),
             "rationale": d.get("rationale")}
            for d in decisions[:20]
        ],
        "decisions_count": len(decisions),
        "bim": bim,
        "classification": classification,  # W17.3 хребет (своды по этажам/системам)
        "cde": cde,  # W17.3 контейнеры по состояниям WIP/Shared/Published/Archived
        "lbs": lbs,  # W17.3 захватки-хабы (своды журнала объёмов)
        # PARA-корзины из статусов (не ручные папки): активные задачи / нормативы / закрытое.
        "para": {"active": len(open_tasks), "resources": len(datasets), "archive": closed},
    }
