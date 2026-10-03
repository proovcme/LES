"""Run real local-model acceptance in a new private Light state, without mocks."""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit

import httpx

from tools.light_launcher import LightStack

ROOT = Path(__file__).resolve().parents[1]


def run(qdrant: Path, output: Path, model: str, *, with_documents=False, with_workspace=False, with_deletion=False, root: Path = ROOT):
    output.mkdir(parents=True, exist_ok=True)
    state = Path(tempfile.mkdtemp(prefix="model-journey-", dir=output)).resolve()
    for name in ("data", "storage", "logs", "RAG_Content", "artifacts"):
        (state / name).mkdir()
    receipt = {"status": "running", "state": str(state), "model": model, "steps": []}

    def record(step, result):
        receipt["steps"].append({"step": step, "result": result})
        (state / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2), encoding="utf-8")
        print(step, flush=True)

    stack = LightStack(root.resolve(), state, qdrant.resolve(), read_only=True)
    try:
        ui_url = stack.start(threading.Event())
        record("isolated_runtime_ready", {"ui_url": ui_url, "api_port": stack.api_port})
        with httpx.Client(base_url=f"http://127.0.0.1:{stack.api_port}", timeout=180, trust_env=False) as client:
            def call(method, route, payload=None):
                response = client.request(method, route, json=payload)
                if not response.is_success:
                    record("http_failure", {"route": route, "status": response.status_code, "body": response.text[:3000]})
                    status = client.get("/api/status")
                    if status.is_success:
                        record("failure_admission", status.json().get("chat_admission", {}))
                response.raise_for_status()
                return response.json()

            connection = call("POST", "/api/model-connections", {
                "display_name": "Release test chat", "base_url": "http://127.0.0.1:11434/v1",
                "model_id": model, "locality": "loopback", "requested_context_tokens": 8192, "extension_type": "ollama",
            })
            record("connection_created", connection)
            revision = connection["revision_id"]
            connection_id = connection["connection_id"]
            probe = call("POST", f"/api/model-connections/{connection_id}/test", {
                "revision_id": revision, "capabilities": ["chat_completions", "streaming", "tools"],
            })
            record("real_model_probe", probe)
            record("answer_binding", call("PUT", "/api/model-connections/roles/answer", {"connection_revision_id": revision}))
            question = "Напиши краткое дружелюбное приветствие по-русски. Документы для этого не нужны."
            with httpx.Client(timeout=180, trust_env=False) as provider:
                baseline = provider.post("http://127.0.0.1:11434/api/chat", json={
                    "model": model, "messages": [{"role": "user", "content": question}],
                    "think": False, "stream": False,
                    "options": {"num_predict": 2048, "num_ctx": 8192, "temperature": 0},
                })
                baseline.raise_for_status()
                record("direct_model_baseline", baseline.json())
                if not baseline.json().get("message", {}).get("content", "").strip() or baseline.json().get("done_reason") == "length":
                    raise AssertionError("The direct model baseline did not finish a visible answer")
            answer = call("POST", "/api/chat", {"question": question, "mode": "agent"})
            record("product_chat", answer)
            if not str(answer.get("answer") or "").strip():
                raise AssertionError("The product did not return a visible answer")
            if with_documents:
                embedding = call("POST", "/api/model-connections", {
                    "display_name": "Release test embeddings", "base_url": "http://127.0.0.1:11434/v1",
                    "model_id": "bge-m3", "locality": "loopback", "extension_type": "ollama",
                })
                record("embedding_probe", call("POST", f"/api/model-connections/{embedding['connection_id']}/test", {
                    "revision_id": embedding["revision_id"], "capabilities": ["embeddings"],
                }))
                record("embedding_binding", call("PUT", "/api/model-connections/roles/embeddings", {"connection_revision_id": embedding["revision_id"]}))
                dataset = call("POST", "/api/rag/datasets", {"name": "Приёмка 🌲 — библиотека"})
                dataset_id = dataset["id"]
                fixture = "Проект «Зелёная библиотека». Открытие библиотеки назначено на 17 мая 2031 года. Ответственная за открытие — Марина Лесная. Это вымышленный документ для проверки приложения."
                upload = client.post(f"/api/rag/upload/{dataset_id}", files={"file": ("Открытие #1.txt", fixture.encode("utf-8"), "text/plain")})
                upload.raise_for_status()
                record("document_uploaded", upload.json())
                deadline = time.monotonic() + 180
                while True:
                    docs = call("GET", f"/api/rag/documents?dataset_id={dataset_id}")
                    rows = docs.get("documents") or []
                    if rows and rows[0]["status"] == "ERROR":
                        record("indexing_failed", docs)
                        raise AssertionError("The uploaded document failed indexing")
                    if rows and rows[0]["status"] == "INDEXED" and rows[0]["chunk_count"] > 0:
                        record("document_indexed", docs)
                        break
                    if time.monotonic() > deadline:
                        record("indexing_timeout", docs)
                        raise TimeoutError("The test document was not indexed")
                    time.sleep(1)
                question = "Когда откроется Зелёная библиотека и кто отвечает за открытие?"
                search = call("POST", "/api/search", {"query": question, "dataset_ids": [dataset_id], "include_trace": True})
                record("document_search", search)
                if not search.get("chunks"):
                    raise AssertionError("The indexed fixture was not retrieved")
                readiness = call("GET", f"/api/rag/readiness?dataset_id={dataset_id}&force=true")
                record("native_search_readiness", readiness)
                if readiness.get("status") != "ok" or not readiness.get("general", {}).get("rrf_ready"):
                    raise AssertionError("Search readiness did not confirm the indexed fixture")
                grounded = call("POST", "/api/chat", {"question": question, "dataset_ids": [dataset_id], "mode": "search"})
                record("grounded_answer", grounded)
                if not grounded.get("source_map"):
                    raise AssertionError("The document answer has no source map")
                from sovushka.answer_render import citation_drawer_item, link_source_markers, source_marker_numbers

                markers = source_marker_numbers(grounded.get("answer") or "")
                if not markers or any(number > len(grounded["source_map"]) for number in markers):
                    raise AssertionError("The answer did not cite an available source")
                rendered = link_source_markers(grounded["answer"], source_count=len(grounded["source_map"]),
                                              sources=grounded["source_map"])
                if "Открытие #1.txt](#source-" not in rendered:
                    raise AssertionError("The real answer did not render a named document link")
                record("named_citation_verified", {"source_numbers": markers, "named_link": True})

                source = citation_drawer_item(grounded["source_map"][0])
                route = source.get("open_url") or ""
                if not route.startswith("/lite-api/documents/by-id/"):
                    raise AssertionError("The answer has no typed same-origin source link")
                origin = urlsplit(ui_url)
                with httpx.Client(base_url=f"{origin.scheme}://{origin.netloc}", timeout=30, trust_env=False) as browser_client:
                    original = browser_client.get(route)
                    original.raise_for_status()
                    if original.content != fixture.encode("utf-8"):
                        raise AssertionError("The source link returned a different document")
                record("source_original_verified", {"route": route, "bytes": len(original.content), "exact_match": True,
                                                     "scope": "HTTP through the UI bridge; not a visual click test"})
            if with_workspace:
                project = call("POST", "/api/projects", {"name": "Проверка 🌲 — память"})
                project_id = project["id"]
                session = call("POST", "/api/workspace/sessions", {"project_id": project_id, "title": "Тестовый чат"})
                sid = session["session_id"]
                changed = call("PATCH", f"/api/workspace/sessions/{sid}", {"title": "Переименованный чат"})
                assert changed["title"] == "Переименованный чат"
                note = call("POST", "/api/workspace/memory", {"text": "Контрольная заметка", "project_id": project_id})
                memory_url = f"/api/workspace/memory/{note['id']}"
                changed = call("PATCH", memory_url, {"project_id": project_id, "text": "Изменённая заметка", "enabled": False})
                assert changed["text"] == "Изменённая заметка" and not changed["enabled"]
                assert not call("GET", "/api/workspace/memory")["notes"]
                assert call("DELETE", f"{memory_url}?project_id={project_id}")["deleted"]
                registry = call("GET", "/api/profiles")
                active = next(item["active"] for item in registry["profiles"] if item["mode"] == "agent")
                text_v1 = "При использовании документов указывай источники."
                skill = call("POST", "/api/profiles/text-revisions", {"kind": "skill", "name": "Тестовый скилл", "text": text_v1})
                skill_v2 = call("POST", "/api/profiles/text-revisions", {"kind": "skill", "name": "Тестовый скилл", "text": text_v1 + " Сохраняй ссылки.", "source_revision_id": skill["revision_id"]})
                assert skill_v2["revision_id"] != skill["revision_id"]
                profile = call("POST", "/api/profiles/revisions", {"mode": "agent", "name": "Проверка версий",
                    "prompt_revision_id": active["prompt_revision_id"], "skill_revision_id": skill["revision_id"], "tools": active["tools"]})
                binding = call("PUT", f"/api/profiles/chats/{sid}/binding", {"mode": "agent", "profile_revision_id": profile["revision_id"]})
                assert binding["skill_text"] == text_v1
                record("workspace_crud_verified", {"project_id": project_id, "session_id": sid,
                    "memory_scope_edit_delete": True, "skill_revisions_distinct": True,
                    "explicit_binding_preserved_selected_text": True,
                    "scope": "real API CRUD; model prompt trace and UI remain separate"})
            if with_deletion:
                if not with_documents:
                    raise ValueError("Deletion acceptance requires its own synthetic document fixture")
                control = call("POST", "/api/rag/datasets", {"name": "Контрольный датасет — не удалять"})
                deleted = call("DELETE", f"/api/rag/datasets/{dataset_id}")
                assert deleted.get("status") == "deleted" and deleted.get("recovery", {}).get("directory")
                remaining = call("GET", "/api/rag/datasets")
                record("dataset_deletion_result", {"deleted": deleted, "remaining": remaining, "control_id": control["id"]})
                rows = remaining if isinstance(remaining, list) else remaining.get("datasets", [])
                assert all(row["id"] != dataset_id for row in rows)
                assert any(row["id"] == control["id"] for row in rows)
        receipt["status"] = "needs_semantic_review"
        record("completed", {"scope": "real connection, role binding, plain chat" +
                             (", document indexing, RAG and exact original through UI bridge" if with_documents else "") +
                             (", project/session/memory/skill/profile API operations" if with_workspace else "")})
        return receipt
    except Exception as error:
        receipt["status"] = "failed"
        record("failure", {"type": type(error).__name__, "message": str(error)})
        raise
    finally:
        stack.stop()


if __name__ == "__main__":
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--qdrant", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--model", default="qwen3.5:2b")
    parser.add_argument("--with-documents", action="store_true")
    parser.add_argument("--with-workspace", action="store_true")
    parser.add_argument("--with-deletion", action="store_true")
    parser.add_argument("--root", type=Path, default=ROOT, help="Exact candidate runtime directory")
    args = parser.parse_args()
    result = run(args.qdrant, args.output, args.model, with_documents=args.with_documents, with_workspace=args.with_workspace, with_deletion=args.with_deletion, root=args.root)
    print(json.dumps({"status": result["status"], "state": result["state"]}))
