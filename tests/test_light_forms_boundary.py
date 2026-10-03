"""Generic document forms remain usable without the inherited estimate forms."""

import pytest
from fastapi import HTTPException

from proxy.routers import forms


@pytest.mark.asyncio
async def test_light_forms_list_hides_specialized_forms(monkeypatch):
    monkeypatch.setattr(forms.forms_service, "list_forms", lambda: [
        {"id": "technical_letter"}, {"id": "ks2"}, {"id": "smeta_lsr"},
    ])
    assert await forms.forms_list(_user=object()) == {"forms": [{"id": "technical_letter"}]}


@pytest.mark.asyncio
async def test_light_forms_reject_specialized_ids_before_work(monkeypatch):
    def unexpected(*_args, **_kwargs):
        raise AssertionError("specialized form reached a service")

    monkeypatch.setattr(forms.forms_service, "resolve_fields", unexpected)
    monkeypatch.setattr(forms.forms_service, "generate", unexpected)
    monkeypatch.setattr(forms.list_office_service, "create_draft", unexpected)
    monkeypatch.setattr(forms.list_office_agent_service, "prepare_document_ir", unexpected)
    actions = (
        forms.forms_fields("ks2", _user=object()),
        forms.forms_generate("smeta_lsr", forms.FormGenerate(), _user=object()),
        forms.forms_download("vor", "old.xlsx", _user=object()),
        forms.office_artifact_create(forms.OfficeDraftCreate(form_id="ks3"), _user=object()),
        forms.office_agent_draft(forms.OfficeAgentDraft(form_id="ks6a"), _user=object()),
    )
    for action in actions:
        with pytest.raises(HTTPException) as error:
            await action
        assert error.value.status_code == 404


@pytest.mark.asyncio
async def test_light_generic_form_still_generates(monkeypatch):
    monkeypatch.setattr(forms.forms_service, "generate", lambda *_args, **_kwargs: {
        "path": None, "resolved": {"id": "technical_letter"},
    })
    result = await forms.forms_generate("technical_letter", forms.FormGenerate(), _user=object())
    assert result["resolved"]["id"] == "technical_letter"
