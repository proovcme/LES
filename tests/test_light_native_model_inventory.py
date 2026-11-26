"""RVT inventory uses the scoped index in chat and public source calls."""
from contextlib import closing
from hashlib import sha256
import json
import sqlite3

import pytest
from qdrant_client import QdrantClient, models

from tests.test_light_native_chat_turn import native_turn


SCHEMA = "les.rvt-record.v1"


def point(dataset, file, digest, section, native_id, parameters, *, typ=None, systems=()):
    model = {"provider": "les", "reader": {"format": "rvt"}, "section": section, "native_id": native_id,
             "source_sha256": digest, "record_schema": SCHEMA, "coverage": {"status": "partial"}}
    model["model_context"] = {"systems": [{"native_id": "18", "name": name} for name in systems]}
    if typ:
        model["model_context"]["types"] = [{"native_id": typ, "name": "3/4''"}]
        model["model_context"]["levels"] = [{"native_id": "1", "name": "Этаж 01"}]
    key = sha256(json.dumps([SCHEMA, dataset, file, digest, section, native_id],
                            separators=(",", ":")).encode()).hexdigest()
    return dict(dataset_id=dataset, file_name=file, source_sha256=digest, node_role="evidence",
                engineering_record_key=key, model_locator=model,
                indexed_engineering_record={"schema": SCHEMA, "name": None, "parameters": parameters})


@pytest.mark.parametrize("native_turn", ["public"], indirect=True)
def test_rvt_counts_reads_the_registered_snapshot_with_no_original_or_vector_index(native_turn):
    turn = native_turn
    url = "/api/rag/sources/call"
    dataset = turn.client.post("/api/rag/datasets", json={"name": "RVT snapshot without Qdrant"}).raise_for_status().json()["id"]
    digest = "a" * 64
    with sqlite3.connect(turn.database) as db:
        db.execute("INSERT INTO documents(id,dataset_id,file_name,status,file_hash,chunk_count,source_path) "
                   "VALUES(?,?,?,'INDEXED',?,1,?)",
                   ("rvt-doc", dataset, "fixture.rvt", digest, str(turn.state / "unavailable.rvt")))

    def query(**options):
        return turn.client.post(url, json={"tool":"model_inventory", "args":{
            "dataset_id":dataset, "mode":"rvt_counts", "doc_id":"rvt-doc", "category_id":-2008099, **options,
        }})

    legacy = query()
    assert legacy.status_code == 409 and "RVT_SNAPSHOT_NOT_READY" in legacy.text
    snapshot = {"schema":"les.rvt-count-records.v1", "source_sha256":digest,
                "reader":{"format":"rvt","revision":"test-reader"}, "coverage":{"status":"partial"},
                "objects":[{"object_id":f"elements:{identifier}", "native_id":identifier,
                            "section":"elements","category_id":category,
                            "model_context":{"types":[{"native_id":"40","name":"Клапан"}],
                                             "levels":[{"native_id":"12","name":"Этаж 2"}]}}
                           for identifier, category in [("41",-2008099),("42",-2008099),("77",-2000280)]]}
    with sqlite3.connect(turn.database) as db:
        db.execute("CREATE TABLE les_rvt_count_snapshots(doc_id TEXT,source_sha256 TEXT,payload BLOB)")
        db.execute("INSERT INTO les_rvt_count_snapshots VALUES(?,?,?)",
                   ("rvt-doc", digest, json.dumps(snapshot).encode("utf-8")))

    saved = query().raise_for_status().json()
    assert saved["result"]["matched_instances"] == 2
    assert saved["result"]["rows"][0]["instance_ids"] == ["41","42"]
    assert saved["result"]["rows"][0]["level_name"] == "Этаж 2"
    assert saved["sources"][0]["model_locator"]["native_id"] == "41"
    assert saved["sources"][0]["model_locator"]["source_sha256"] == digest
    assert query(index_revision="prior-version").status_code == 409
    with sqlite3.connect(turn.database) as db:
        db.execute("UPDATE documents SET file_hash=? WHERE id='rvt-doc'", ["b"*64])
    modified = query()
    assert modified.status_code == 409 and "RVT_SNAPSHOT_NOT_READY" in modified.text


def parameter(name, value, **attributes):
    return dict(name=name, value=value, **attributes)


@pytest.mark.parametrize("native_turn", ["rag-small"], indirect=True)
def test_model_inventory_joins_and_counts_only_after_the_model_calls_it(native_turn):
    turn = native_turn
    url = "/api/rag/sources/call"
    scoped = turn.client.post("/api/rag/datasets", json={"name": "Model in scope"}).raise_for_status().json()["id"]
    other = turn.client.post("/api/rag/datasets", json={"name": "Separate model"}).raise_for_status().json()["id"]
    file_name = "fixture.rvt"
    digest = "a" * 64
    with sqlite3.connect(turn.database) as db:
        db.executemany("INSERT INTO documents(id,dataset_id,file_name,status,file_hash,chunk_count) VALUES(?,?,?,'INDEXED',?,1)",
                       [("scoped-doc", scoped, file_name, digest), ("other-doc", other, file_name, digest)])
    nominal = parameter("ADSK_Номинальная мощность", 26909.776041774305, storage_type="Double",
                        spec_type_id="autodesk.spec.aec.electrical:power-1.0.0", parameter_id=182473)
    known = [parameter("ADSK_Наименование", "Кран шаровый"), parameter("ADSK_Артикул", "AB-12"),
             parameter("ADSK_Завод-изготовитель", "Fixture maker"), nominal]
    points = [
        point(scoped, file_name, digest, "elements", "5", [nominal], typ="7", systems=["В1 1"]),
        point(scoped, file_name, digest, "elements", "6", [], typ="7", systems=["В1 2"]),
        point(scoped, file_name, digest, "types", "7", known),
        point(scoped, file_name, "b" * 64, "types", "7", [parameter("ADSK_Наименование", "Old type")]),
        point(scoped, file_name, digest, "elements", "8", [parameter("ADSK_Наименование", "Насос"),
            parameter("ADSK_Номинальная мощность", 42, display_unit_id="autodesk.unit.unit:watts-1.0.1")], systems=["В1 1"]),
        point(other, file_name, digest, "types", "7", [parameter("ADSK_Наименование", "Other model")]),
        point(other, file_name, digest, "elements", "5", [], typ="7", systems=["В1 1"]),
    ]
    with closing(QdrantClient(url=turn.env["LES_LIGHT_QDRANT_URL"], api_key=turn.env["LES_LIGHT_QDRANT_API_KEY"],
                              check_compatibility=False, trust_env=False)) as qdrant:
        qdrant.create_collection("docs", vectors_config=models.VectorParams(size=2, distance=models.Distance.COSINE))
        qdrant.upsert("docs", [models.PointStruct(id=index, vector=[1., 0.], payload=payload)
                              for index, payload in enumerate(points, 1)], wait=True)

    def query(**args):
        return turn.client.post(url, json={"tool": "model_inventory", "args": {"dataset_id": scoped, **args}})

    listing = query(mode="list", q="краны").raise_for_status().json()
    assert listing["result"]["matched_instances"] == 2
    assert listing["result"]["complete_model"] is False
    assert [row["element_id"] for row in listing["result"]["rows"]] == ["5", "6"]
    assert listing["result"]["rows"][0]["fields"]["name"]["element_id"] == "7"
    assert all(s["doc_id"] == "scoped-doc" for s in listing["sources"])
    vendor = query(mode="vendors").raise_for_status().json()["result"]["rows"]
    assert len(vendor) == 1 and vendor[0]["quantity"] == 2 and vendor[0]["supplier"] is None
    assert vendor[0]["type_id"] == "7"
    power = query(mode="power", system="В1 1").raise_for_status().json()["result"]["power"]
    assert power["nominal_w"] == {"sum": 2500.0, "known": 1, "unknown": 1, "conflicts": 0}
    assert power["apparent_va"]["sum"] is None
    assert query(mode="list", doc_id="other-doc").status_code == 403
    assert query(mode="list", offset=1).status_code == 422
    assert query(mode="list", index_revision="foreign-revision").status_code == 409

    scope = {"scope_type": "dataset", "dataset_ids": [scoped]}
    former_trigger = "Перечисли все краны в модели"
    turn.scripts.append({"text": "Ответ модели без инструмента."})
    untouched = turn.client.post("/api/chat", json={"question": former_trigger, "mode": "agent",
        "retrieval_mode": "pull", "scope": scope}).raise_for_status().json()
    assert untouched["answer"] == "Ответ модели без инструмента."
    assert untouched["artifact"]["tool_results"] == []
    assert len(turn.seen) == 1 and former_trigger in str(turn.seen[0]["messages"])

    question = "Какая там запорная арматура? Покажи её с этажами."
    def select_tool(body):
        assert question in str(body["messages"])
        assert any(t["function"]["name"] == "model_inventory" for t in body["tools"])
        return {"tool": "model_inventory", "args": {"mode": "list", "q": "краны"}}
    def answer_from_tool(body):
        result = next(json.loads(m["content"]) for m in body["messages"] if m["role"] == "tool")
        assert result["status"] == "ok", result
        assert result["result"]["matched_instances"] == 2
        row = result["result"]["rows"][0]
        assert row["name"] == "Кран шаровый"
        source = next(s for s in result["sources"] if s["index"] == row["source_index"])
        assert source["label"] == f'[Источник {row["source_index"]}]'
        assert row["citation"] == source["label"]
        return {"text": f'Кран шаровый, Этаж 01. {source["label"]}'}

    body = {"question": question, "mode": "search", "retrieval_mode": "pull",
            "scope": scope}
    turn.scripts.extend([select_tool, answer_from_tool])
    reply = turn.client.post("/api/chat", json=body)
    assert reply.is_success, reply.text
    chat = reply.json()
    assert chat["query_route"]["channel"] == "generic"
    assert "Кран шаровый" in chat["answer"] and "Этаж 01" in chat["answer"]
    assert chat["artifact"]["tool_results"][0]["operation"] == "list"
    assert {s["model_locator"]["native_id"] for s in chat["source_map"]} == {"5", "6"}
    turn.scripts.extend([select_tool, answer_from_tool])
    stream = turn.client.post("/api/chat/stream", json=body).raise_for_status().text
    assert "event: final" in stream and "event: sources" in stream and "event: error" not in stream
    assert len(turn.seen) == 5 and turn.embeddings.calls == []


@pytest.mark.parametrize("native_turn", ["rag-small"], indirect=True)
def test_dwg_inventory_counts_model_blocks_and_keeps_unknown_power_separate(native_turn):
    turn = native_turn
    url = "/api/rag/sources/call"
    dataset = turn.client.post("/api/rag/datasets", json={"name": "Lighting drawing"}).raise_for_status().json()["id"]
    file, digest = "lighting.dwg", "a" * 64
    with sqlite3.connect(turn.database) as db:
        db.execute("INSERT INTO documents(id,dataset_id,file_name,status,file_hash,chunk_count) VALUES(?,?,?,'INDEXED',?,1)",
                   ("dwg-doc", dataset, file, digest))

    def insert(handle, name, layer, quantity, power):
        schema = "les.dwg-insert.v1"
        payload = point(dataset, file, digest, "elements", handle, [])
        locator = payload["model_locator"]
        locator.update(reader={"format": "dwg"}, record_schema=schema, identifiers={"handle": handle})
        payload["engineering_record_key"] = sha256(json.dumps([schema, dataset, file, digest, "elements", handle],
                                  separators=(",", ":")).encode()).hexdigest()
        payload["indexed_engineering_record"] = {"schema": schema, "name": name, "layer": layer,
            "quantity": quantity, "space": "model", "owner_handle": "17",
            "attributes": [{"tag": "POWER_W", "value": value} for value in power]}
        return payload

    legacy = point(dataset, file, digest, "elements", "AF", [])
    legacy.pop("engineering_record_key")
    legacy.pop("indexed_engineering_record")
    points = [insert("A1", "Fixture", "EO-LIGHT", 3, ["0,018 kW"]),
              insert("A2", "Fixture", "EO-LIGHT", 1, ["18", "21"]),
              insert("A3", "Exit", "EO-EMERG", 1, ["3"]),
              insert("A4", "Socket", "EM-POWER", 1, ["500"]), legacy]
    with closing(QdrantClient(url=turn.env["LES_LIGHT_QDRANT_URL"], api_key=turn.env["LES_LIGHT_QDRANT_API_KEY"],
                              check_compatibility=False, trust_env=False)) as qdrant:
        qdrant.create_collection("docs", vectors_config=models.VectorParams(size=2, distance=models.Distance.COSINE))
        qdrant.upsert("docs", [models.PointStruct(id=i, vector=[1., 0.], payload=p)
                                for i, p in enumerate(points, 1)], wait=True)

    def query(**args):
        return turn.client.post(url, json={"tool": "model_inventory", "args": {
            "mode": "dwg_blocks", "dataset_id": dataset, **args}}).raise_for_status().json()

    response = query(lighting_only=True)
    result = response["result"]
    assert result["matched_instances"] == 5 and result["complete_model"] is False
    assert result["power"] == {"sum_w": 57., "known": 4, "unknown": 1, "attribute": "POWER_W"}
    assert {row["name"]: row["quantity"] for row in result["rows"]} == {"Fixture": 4, "Exit": 1}
    assert all(s["model_locator"]["native_id"] == s["model_locator"]["identifiers"]["handle"] for s in response["sources"])
    assert query(block="fixture", layer="EO-LIGHT")["result"]["matched_instances"] == 4
    assert query(layer="EM-POWER")["result"]["power"]["sum_w"] == 500.
    turn.scripts.extend([{"tool": "model_inventory", "args": {"mode": "dwg_blocks", "lighting_only": True}},
                         {"text": "По заполненным атрибутам 57 Вт. [Источник 1]"}])
    chat = turn.client.post("/api/chat", json={"question": "Сколько ватт в сумме у светотехники?",
        "mode": "search", "retrieval_mode": "pull", "scope": {"scope_type": "dataset", "dataset_ids": [dataset]}})
    assert chat.is_success, chat.text
    chat = chat.json()
    assert chat["query_route"]["channel"] == "generic"
    assert chat["artifact"]["tool_results"][0]["operation"] == "dwg_blocks"
    assert {s["model_locator"]["native_id"] for s in chat["source_map"]} == {
        s["model_locator"]["native_id"] for s in response["sources"]}
    assert "57 Вт" in chat["answer"] and len(turn.seen) == 2 and turn.embeddings.calls == []
