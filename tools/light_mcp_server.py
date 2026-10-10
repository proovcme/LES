"""Read-only MCP gateway to the running LES RAG API.

The gateway intentionally exposes only public document operations. It never
starts Qdrant, opens a second state directory, or exposes inherited LES tools.
Run it as a stdio server from an MCP client while LES RAG is running.
"""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from urllib.parse import quote, urlencode, urlsplit

import httpx


def _state_root() -> Path:
    configured = os.environ.get("LES_LIGHT_STATE", "").strip()
    if configured:
        return Path(configured)
    if sys.platform == "darwin":
        return Path.home() / "Library/Application Support/LES Light"
    local = os.environ.get("LOCALAPPDATA", "").strip()
    if not local:
        raise RuntimeError("Укажите LES_LIGHT_STATE — папку данных запущенного LES RAG")
    return Path(local) / "LES Light"


def _running_api() -> str:
    status_file = _state_root() / "launcher-status.json"
    try:
        status = json.loads(status_file.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise RuntimeError("LES RAG не запущен. Откройте приложение и повторите запрос") from error
    if status.get("schema") != "les.light-launcher.v1" or status.get("phase") != "ready" or not status.get("instance_id"):
        raise RuntimeError("LES RAG ещё не готов. Дождитесь запуска приложения")
    url = str(status.get("api_url") or "")
    parsed = urlsplit(url)
    if parsed.scheme != "http" or parsed.hostname not in {"127.0.0.1", "localhost"} or not parsed.port or parsed.username or parsed.password or parsed.path not in {"", "/"} or parsed.query or parsed.fragment:
        raise RuntimeError("Адрес API в состоянии LES RAG некорректен")
    return f"http://127.0.0.1:{parsed.port}"


def _request(method: str, path: str, *, params: dict | None = None, body: dict | None = None, timeout: float = 25) -> dict:
    base = _running_api()
    headers = {}
    key = os.environ.get("LES_LIGHT_MCP_API_KEY", "").strip()
    if key:
        headers["X-API-Key"] = key
    try:
        with httpx.Client(timeout=timeout, trust_env=False, follow_redirects=False, headers=headers) as client:
            identity = client.get(base + "/api/light/instance")
            identity.raise_for_status()
            expected = json.loads((_state_root() / "launcher-status.json").read_text(encoding="utf-8")).get("instance_id")
            if not expected or identity.json().get("instance_id") != expected:
                raise RuntimeError("Экземпляр LES RAG изменился. Повторите запрос после запуска")
            response = client.request(method, base + path, params=params, json=body)
            response.raise_for_status()
            return response.json()
    except httpx.HTTPStatusError as error:
        if error.response.status_code in {401, 403}:
            raise RuntimeError("Нет доступа к данным LES RAG. Проверьте ключ MCP-подключения") from error
        raise RuntimeError(f"LES RAG вернул отказ {error.response.status_code}. Проверьте состояние приложения") from error
    except (httpx.HTTPError, ValueError, OSError) as error:
        raise RuntimeError("LES RAG не ответил. Проверьте, что приложение запущено") from error


def list_datasets() -> dict:
    """List user document sets and their factual index counts."""
    data = _request("GET", "/api/documents/datasets", params={"limit": 1000})
    rows = []
    for item in data.get("datasets") or []:
        if str(item.get("dataset_scope") or "user") == "system":
            continue
        rows.append({
            "id": str(item.get("id") or ""),
            "name": str(item.get("display_name") or item.get("name") or ""),
            "documents": int(item.get("document_count") or 0),
            "declared_chunks": int(item.get("chunk_count") or 0),
        })
    return {"schema": "les.light.mcp.datasets.v1", "datasets": rows}


def search_sources(query: str, dataset_ids: list[str] | None = None, limit: int = 6) -> dict:
    """Search indexed documents through LES RAG's native RRF retrieval, with source links."""
    query = query.strip()
    if not query or len(query) > 4000:
        raise ValueError("Введите поисковый запрос длиной до 4000 символов")
    if not 1 <= limit <= 20:
        raise ValueError("Количество результатов должно быть от 1 до 20")
    catalog = {row["id"]: row for row in list_datasets()["datasets"]}
    allowed = set(catalog)
    selected = list(dict.fromkeys(dataset_ids if dataset_ids is not None else sorted(allowed)))
    if any(item not in allowed for item in selected):
        raise ValueError("Набор не найден среди пользовательских данных LES RAG")
    if not selected:
        return {"schema": "les.light.mcp.search.v1", "query": query, "count": 0, "hits": [], "retrieval": {"status": "empty"}}
    if not any(catalog[item]["declared_chunks"] > 0 for item in selected):
        return {
            "schema": "les.light.mcp.search.v1", "query": query, "count": 0, "hits": [],
            "retrieval": {"status": "not_ready", "message":
                          "В выбранных наборах пока нет поисковых фрагментов. Проверьте состав и обработку документов в LES RAG."},
        }
    result = _request("POST", "/api/search", body={
        "query": query, "dataset_ids": selected, "top_k": limit,
        "max_chars": 1600, "include_context": True, "include_trace": True,
    })
    api = _running_api()
    hits = []
    for chunk in result.get("chunks") or []:
        meta = chunk.get("metadata") or {}
        dataset_id = str(meta.get("dataset_id") or "")
        doc_name = str(chunk.get("doc_name") or "")
        doc_id = str(chunk.get("doc_id") or "")
        link = ""
        if dataset_id and doc_name:
            path_id = quote(doc_id or "unknown", safe="")
            link = f"{api}/api/documents/by-id/{path_id}/viewer?" + urlencode({"dataset_id": dataset_id, "doc_name": doc_name})
        hits.append({
            "rank": chunk.get("rank"), "document": doc_name,
            "dataset_id": dataset_id, "document_id": doc_id,
            "page": meta.get("page"), "section": meta.get("section_heading") or meta.get("parent_heading"),
            "excerpt": str(chunk.get("content") or ""),
            "context": str((chunk.get("context") or {}).get("content") or ""),
            "context_fragments": [{"excerpt": item.get("content", ""),
                "page": (item.get("metadata") or {}).get("page"),
                "point_id": (item.get("metadata") or {}).get("qdrant_point_id")}
                for item in ((chunk.get("context") or {}).get("metadata") or {}).get("context_fragments", [])],
            "original_url": link,
        })
    trace = result.get("retrieval_trace") or {}
    status = str(trace.get("status") or "unknown")
    error_code = str(trace.get("error_code") or "") if status != "ok" else ""
    problem = ""
    if status == "blocked":
        problem = {
            "native_rrf_failed": "Поиск по индексу сейчас недоступен. Проверьте состояние Qdrant и готовность документов в LES RAG.",
            "query_embedding_failed": "Не удалось получить вектор запроса. Проверьте назначенную модель поиска в LES RAG.",
        }.get(error_code, "Поиск сейчас недоступен. Проверьте состояние набора и модели поиска в LES RAG.")
    return {
        "schema": "les.light.mcp.search.v1", "query": query,
        "count": len(hits), "hits": hits,
        "retrieval": {"fusion": trace.get("fusion"), "status": status,
                      "error_code": error_code, "message": problem},
    }


def build_server():
    from mcp.server.fastmcp import FastMCP
    from mcp.types import ToolAnnotations

    server = FastMCP("LES RAG")
    read_only = ToolAnnotations(readOnlyHint=True, destructiveHint=False, idempotentHint=True)
    server.tool(name="list_datasets", description="Список пользовательских наборов LES RAG", annotations=read_only)(list_datasets)
    server.tool(name="search_sources", description="Поиск по документам LES RAG с фрагментами и ссылками на оригиналы", annotations=read_only)(search_sources)
    return server


def main() -> None:
    build_server().run()


if __name__ == "__main__":
    main()
