"""Exercise independent state transitions against an isolated real Light runtime."""
from __future__ import annotations

import argparse
import inspect
import json
from pathlib import Path
import sqlite3
import tempfile
import threading

import httpx

from tools.light_launcher import LightStack


def run(root: Path, qdrant: Path, output: Path) -> dict:
    imported_root = Path(inspect.getfile(LightStack)).resolve().parents[1]
    if imported_root != root.resolve():
        raise ValueError(f"Runtime mismatch: Python imports {imported_root}, requested {root.resolve()}")
    output.mkdir(parents=True, exist_ok=True)
    state = Path(tempfile.mkdtemp(prefix="state-journey-", dir=output)).resolve()
    for name in ("data", "storage", "logs", "RAG_Content", "artifacts"):
        (state / name).mkdir()
    receipt = {"state": str(state), "runtime": str(root.resolve()), "scenarios": []}
    stack = LightStack(root.resolve(), state, qdrant.resolve(), read_only=True)

    def save():
        (state / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2), encoding="utf-8")

    def scenario(name, action):
        try:
            evidence = action()
            row = {"name": name, "status": "passed", "evidence": evidence}
        except Exception as error:
            row = {"name": name, "status": "failed", "error": f"{type(error).__name__}: {error}"}
        receipt["scenarios"].append(row)
        save()
        print(f"{name}: {row['status']}", flush=True)

    def call(method, route, body=None, expected=200):
        with httpx.Client(base_url=f"http://127.0.0.1:{stack.api_port}", timeout=60, trust_env=False) as client:
            response = client.request(method, route, json=body)
        assert response.status_code == expected, f"{method} {route}: expected {expected}, got {response.status_code}: {response.text[:1200]}"
        return response.json()

    def datasets():
        result = call("GET", "/api/rag/datasets")
        return result if isinstance(result, list) else result["datasets"]

    try:
        stack.start(threading.Event())
        control = call("POST", "/api/rag/datasets", {"name": "Control"})
        empty = call("POST", "/api/rag/datasets", {"name": "Empty"})

        def delete_empty():
            result = call("DELETE", f"/api/rag/datasets/{empty['id']}")
            assert result["status"] == "deleted" and result["recovery"]["directory"]
            ids = {row["id"] for row in datasets()}
            assert empty["id"] not in ids and control["id"] in ids
            return {"recovery": result["recovery"], "control_preserved": True}

        scenario("delete_empty_before_first_index", delete_empty)
        for label, name in (("blank", "  "), ("too_long", "x" * 121), ("control_character", "bad\x01name")):
            scenario(f"create_rejects_{label}", lambda name=name: call("POST", "/api/rag/datasets", {"name": name}, expected=400))
            scenario(f"rename_rejects_{label}", lambda name=name: call("PATCH", f"/api/rag/datasets/{control['id']}/name", {"name": name}, expected=400))

        literal = 'Лес 🌲 " # & %20 < > / \\'

        def literal_create():
            result = call("POST", "/api/rag/datasets", {"name": literal})
            assert result["name"] == literal, f"Input changed: {result['name']!r}"
            assert next(row for row in datasets() if row["id"] == result["id"])["name"] == literal
            return {"id": result["id"], "name_preserved": True}

        scenario("create_preserves_literal_unicode_and_percent", literal_create)

        def rename():
            result = call("PATCH", f"/api/rag/datasets/{control['id']}/name", {"name": literal})
            assert result["id"] == control["id"] and result["name"] == literal
            return {"id_unchanged": True}

        scenario("rename_preserves_identity_and_literal_text", rename)
        project = call("POST", "/api/projects", {"name": "Persistence 🌲"})
        session = call("POST", "/api/workspace/sessions", {"project_id": project["id"], "title": "Before restart"})
        call("PATCH", f"/api/workspace/sessions/{session['session_id']}", {"title": "After edit 🌲"})
        note = call("POST", "/api/workspace/memory", {"project_id": project["id"], "text": "Memory before restart"})
        call("PATCH", f"/api/workspace/memory/{note['id']}", {"project_id": project["id"], "text": "Edited memory 🌲", "enabled": False})
        registry = call("GET", "/api/profiles")
        active = next(row["active"] for row in registry["profiles"] if row["mode"] == "agent")
        skill = call("POST", "/api/profiles/text-revisions", {"kind": "skill", "name": "Persistence skill", "text": "Keep source links."})
        skill2 = call("POST", "/api/profiles/text-revisions", {"kind": "skill", "name": "Persistence skill", "text": "Second revision.", "source_revision_id": skill["revision_id"]})
        profile = call("POST", "/api/profiles/revisions", {"mode": "agent", "name": "Persistence profile", "prompt_revision_id": active["prompt_revision_id"], "skill_revision_id": skill["revision_id"], "tools": active["tools"]})
        call("PUT", f"/api/profiles/chats/{session['session_id']}/binding", {"mode": "agent", "profile_revision_id": profile["revision_id"]})
        stack.stop()
        stack = LightStack(root.resolve(), state, qdrant.resolve(), read_only=True)
        stack.start(threading.Event())

        def persisted():
            assert next(row for row in datasets() if row["id"] == control["id"])["name"] == literal
            assert call("GET", f"/api/projects/{project['id']}")["name"] == "Persistence 🌲"
            result = call("GET", f"/api/workspace/sessions/{session['session_id']}")
            assert result["title"] == "After edit 🌲"
            return {"dataset_project_session_persisted": True}

        scenario("restart_preserves_dataset_project_and_session", persisted)

        def memory_persisted():
            notes = call("GET", f"/api/workspace/memory?project_id={project['id']}")["notes"]
            saved = next(row for row in notes if row["id"] == note["id"])
            assert saved["text"] == "Edited memory 🌲" and not saved["enabled"]
            assert not call("GET", "/api/workspace/memory")["notes"]
            call("DELETE", f"/api/workspace/memory/{note['id']}?project_id=0", expected=404)
            assert any(row["id"] == note["id"] for row in call("GET", f"/api/workspace/memory?project_id={project['id']}")["notes"])
            return {"text_and_disabled_preserved": True, "wrong_scope_delete_rejected": True}

        scenario("restart_memory_scope_and_disabled_state", memory_persisted)

        def skill_persisted():
            registry = call("GET", "/api/profiles")
            versions = {row["revision_id"]: row for row in registry["skill_revisions"]}
            assert versions[skill["revision_id"]]["text"] == "Keep source links."
            assert versions[skill2["revision_id"]]["text"] == "Second revision."
            # Read only this journey's synthetic database; do not rebind and hide lost state.
            with sqlite3.connect((state / "data/meta.db").as_uri() + "?mode=ro", uri=True) as conn:
                row = conn.execute("SELECT snapshot_json FROM les_chat_profile_bindings WHERE session_id=?", (session["session_id"],)).fetchone()
            assert row is not None
            snapshot = json.loads(row[0])
            assert snapshot["skill_text"] == "Keep source links."
            return {"both_revisions_preserved": True, "saved_binding_keeps_first_revision": True, "scope": "persisted binding, not actual model prompt"}

        scenario("restart_preserves_skill_versions_and_chat_binding", skill_persisted)

        def folder_lifecycle():
            folder = state / "fixture-folder"
            folder.mkdir()
            file = folder / "Файл #1.txt"
            file.write_text("Synthetic external source", encoding="utf-8")
            dataset = call("POST", "/api/rag/datasets", {"name": "External lifecycle"})
            body = {"path": str(folder), "dataset_id": dataset["id"], "parse": False}
            assert call("POST", "/api/rag/external/check", body)["counts"]["new"] == 1
            call("POST", "/api/rag/external/sync", body)
            assert call("POST", "/api/rag/external/check", body)["pending_changes"] == 0
            hidden = state / "fixture-folder-away"
            assert folder.resolve().parent == state and hidden.resolve().parent == state and not hidden.exists()
            folder.rename(hidden)
            try:
                call("POST", "/api/rag/external/check", body, expected=404)
                call("POST", "/api/rag/external/sync", body, expected=404)
                docs = call("GET", f"/api/rag/documents?dataset_id={dataset['id']}")["documents"]
                assert len(docs) == 1 and docs[0]["status"] != "MISSING"
            finally:
                hidden.rename(folder)
            assert call("POST", "/api/rag/external/check", body)["pending_changes"] == 0
            renamed = folder / "Новое имя %.txt"
            file.rename(renamed)
            counts = call("POST", "/api/rag/external/check", body)["counts"]
            assert counts["new"] == 1 and counts["deleted"] == 1
            renamed.rename(file)
            return {"missing_folder_rejected_without_marking_files_deleted": True, "return_restores_unchanged_scan": True, "rename_detected": True, "scope": "real filesystem and API registration; no model or auto-index"}

        scenario("external_folder_disappearance_return_and_file_rename", folder_lifecycle)
        receipt["status"] = "failed" if any(row["status"] == "failed" for row in receipt["scenarios"]) else "passed"
    except Exception as error:
        receipt["status"] = "blocked"
        receipt["setup_error"] = f"{type(error).__name__}: {error}"
    finally:
        stack.stop()
        save()
    return receipt


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--qdrant", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = run(args.root, args.qdrant, args.output)
    print(json.dumps({"status": result["status"], "state": result["state"]}))
    raise SystemExit(0 if result["status"] == "passed" else 1)
