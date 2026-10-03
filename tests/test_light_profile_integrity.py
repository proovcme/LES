import copy
import pytest
from proxy.services import chat_profile_service as profiles
from proxy.services.chat_evidence_application_service import profile_system_prompt


def test_light_rejects_legacy_estimator_flow_without_changing_saved_revision(monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    snapshot = {"mode": "agent", "rag_policy": {"model_authored_initial_query": True, "system_datasets": ["smeta"]}}
    original = copy.deepcopy(snapshot)
    effective = profiles.effective_profile_snapshot(snapshot)
    assert "model_authored_initial_query" not in effective["rag_policy"]
    assert "system_datasets" not in effective["rag_policy"]
    assert snapshot == original
    assert profiles.resolve_profile_system_dataset_ids(snapshot, current_dataset_ids=["user"], module_resolver=lambda _: pytest.fail("System dataset must not be read")) == ["user"]


def test_multiple_skills_are_bound_verbatim_and_deleting_library_does_not_change_chat(tmp_path, monkeypatch):
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    db = tmp_path / "profiles.db"
    prompt = profiles.publish_text_revision("prompt", name="Роль", text="Будь редактором.", db_path=db)
    skills = [profiles.publish_text_revision("skill", name=name, text=text, db_path=db) for name, text in [("Стиль", "Короткие предложения."), ("Формат", "Сохраняй таблицы.")]]
    revision = profiles.publish_profile_revision(mode="agent", name="Редактор", prompt_revision_id=prompt["revision_id"], skill_revision_id=skills[0]["revision_id"], additional_skill_revision_ids=[skills[1]["revision_id"]], tools=[], model_policy={}, rag_policy={}, db_path=db)
    bound = profiles.resolve_chat_profile(session_id="chat", requested_mode="agent", requested_revision_id=revision["revision_id"], apply_revision=True, db_path=db)
    before = profile_system_prompt(bound, strict=False)
    for text in ("Будь редактором.", "Короткие предложения.", "Сохраняй таблицы."):
        assert before.count(text) == 1
    profiles.delete_revision("skill", skills[1]["revision_id"], db_path=db)
    copied = profiles.publish_text_revision("skill", name="Копия архивной редакции", text="Сохраняй таблицы.", source_revision_id=skills[1]["revision_id"], db_path=db)
    assert copied["source_revision_id"] == skills[1]["revision_id"]
    resumed = profiles.resolve_chat_profile(session_id="chat", requested_mode="agent", db_path=db)
    assert profile_system_prompt(resumed, strict=False) == before
    with pytest.raises(ValueError):
        profiles.publish_profile_revision(mode="agent", name="Удалённый", prompt_revision_id=prompt["revision_id"], skill_revision_id=skills[0]["revision_id"], additional_skill_revision_ids=[skills[1]["revision_id"]], tools=[], model_policy={}, rag_policy={}, db_path=db)


def test_multiple_skill_total_budget(tmp_path):
    db = tmp_path / "profiles.db"
    p = profiles.publish_text_revision("prompt", name="Роль", text="Роль", db_path=db)
    a = profiles.publish_text_revision("skill", name="А", text="а" * 5000, db_path=db)
    b = profiles.publish_text_revision("skill", name="Б", text="б" * 5000, db_path=db)
    with pytest.raises(ValueError, match="profile_text_too_long"):
        profiles.publish_profile_revision(mode="agent", name="Лимит", prompt_revision_id=p["revision_id"], skill_revision_id=a["revision_id"], additional_skill_revision_ids=[b["revision_id"]], tools=[], model_policy={}, rag_policy={}, db_path=db)


def test_visible_statuses_are_human_labels():
    from sovushka.answer_render import evidence_badges, answer_status
    badges = evidence_badges({"RETRIEVED": 1, "BLOCKED": 2})
    assert [item["label"] for item in badges] == ["Найдено", "Недоступно"]
    assert answer_status("private_internal_failure")["label"] == "Статус не определён"
