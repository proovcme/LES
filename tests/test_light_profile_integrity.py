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


@pytest.mark.parametrize('legacy',[False,True])
def test_factory_upgrade_preserves_existing_revision_and_binding(tmp_path,monkeypatch,legacy):
    import json
    import sqlite3
    db=tmp_path/'factory.db'
    contracts=profiles._factory_contracts()
    old_contracts=json.loads(json.dumps(contracts))
    old_contracts['agent']['prompt']='Factory policy before upgrade.'
    monkeypatch.setattr(profiles,'_factory_contracts',lambda:old_contracts)
    old=profiles.resolve_chat_profile(session_id='old-chat',requested_mode='agent',db_path=db)
    if legacy:
        # A pre-760 factory used one stable Base ID. Preserve it as a historical revision.
        old_id=old['revision_id'];old['revision_id']='factory:profile:agent:base'
        with sqlite3.connect(db) as conn:
            conn.execute('UPDATE les_profile_revisions SET revision_id=?,snapshot_json=? WHERE revision_id=?',
                (old['revision_id'],json.dumps(old,ensure_ascii=False),old_id))
            conn.execute('UPDATE les_active_profiles SET profile_revision_id=? WHERE mode=?',(old['revision_id'],'agent'))
            conn.execute('UPDATE les_chat_profile_bindings SET profile_revision_id=?,snapshot_json=? WHERE session_id=?',
                (old['revision_id'],json.dumps(old,ensure_ascii=False),'old-chat'))
    with sqlite3.connect(db) as conn:
        before=conn.execute('SELECT snapshot_json FROM les_chat_profile_bindings WHERE session_id=?',('old-chat',)).fetchone()[0]
        prompt_before=conn.execute('SELECT text_value,sha256 FROM les_prompt_revisions WHERE revision_id=?',
            (old['prompt_revision_id'],)).fetchone()
    monkeypatch.setattr(profiles,'_factory_contracts',lambda:contracts)
    catalog=profiles.registry_snapshot(db_path=db)
    agent=next(row for row in catalog['profiles'] if row['mode']=='agent')
    assert agent['active_revision_id']==old['revision_id']
    latest=max(agent['revisions'],key=lambda row:row['revision_no'])
    assert latest['revision_id']!=old['revision_id'] and latest['prompt_text']==contracts['agent']['prompt'].strip()
    assert len(agent['revisions'])==2
    # Seeding twice is idempotent and never mutates the embedded chat snapshot.
    assert len(next(row for row in profiles.registry_snapshot(db_path=db)['profiles'] if row['mode']=='agent')['revisions'])==2
    assert profiles.resolve_chat_profile(session_id='old-chat',requested_mode='agent',db_path=db)==old
    with sqlite3.connect(db) as conn:
        assert conn.execute('SELECT snapshot_json FROM les_chat_profile_bindings WHERE session_id=?',('old-chat',)).fetchone()[0]==before
        assert conn.execute('SELECT text_value,sha256 FROM les_prompt_revisions WHERE revision_id=?',
            (old['prompt_revision_id'],)).fetchone()==prompt_before
    profiles.activate_profile_revision('agent',latest['revision_id'],db_path=db)
    assert profiles.resolve_chat_profile(session_id='new-chat',requested_mode='agent',db_path=db)['revision_id']==latest['revision_id']
    assert profiles.resolve_chat_profile(session_id='old-chat',requested_mode='agent',db_path=db)==old


def test_factory_upgrade_does_not_replace_custom_active_selection(tmp_path,monkeypatch):
    import json
    db=tmp_path/'custom.db'
    initial=profiles.resolve_chat_profile(session_id=None,requested_mode='agent',db_path=db)
    custom=profiles.publish_profile_revision(mode='agent',name='Selected by user',
        prompt_revision_id=initial['prompt_revision_id'],skill_revision_id=initial['skill_revision_id'],
        tools=[],model_policy={},rag_policy={},db_path=db)
    profiles.activate_profile_revision('agent',custom['revision_id'],db_path=db)
    changed=json.loads(json.dumps(profiles._factory_contracts()))
    changed['agent']['prompt']='New factory policy.'
    monkeypatch.setattr(profiles,'_factory_contracts',lambda:changed)
    result=profiles.resolve_chat_profile(session_id='fresh',requested_mode='agent',db_path=db)
    assert result['revision_id']==custom['revision_id']
