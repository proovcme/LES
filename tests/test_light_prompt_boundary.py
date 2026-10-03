"""Light's default role must not impose construction workflows on ordinary chat."""
import pytest


@pytest.mark.parametrize("mode", ["search", "agent"])
def test_light_factory_has_no_domain_workflow(monkeypatch, mode):
    from proxy.services.chat_profile_service import _factory_contracts
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    profile = _factory_contracts()[mode]
    text = profile["prompt"] + profile["skill"]
    assert "Для обычного разговора документы не требуются" in text
    for forbidden in ("инженер", "ГЭСН", "Рабочий цикл", "1.", "```json"):
        assert forbidden not in text


def test_light_keeps_user_profile_text_without_extra_grounding_order(monkeypatch):
    from proxy.services.chat_evidence_application_service import profile_system_prompt
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    prompt = profile_system_prompt({"prompt_text": "Пиши стихи.", "skill_text": "Предпочитай рифму."}, strict=False)
    assert "Пиши стихи." in prompt and "Предпочитай рифму." in prompt
    assert "Для проверяемых утверждений используй только реальные материалы текущего запроса" not in prompt


def test_full_factory_role_is_not_available_in_light(monkeypatch):
    from proxy.services.prompt_registry_service import build_factory_mode_system_prompt
    monkeypatch.setenv("LES_PRODUCT_EDITION", "full")
    with pytest.raises(ValueError, match="LES Light only"):
        build_factory_mode_system_prompt("rag")


@pytest.mark.parametrize("question", ["Кратко поздоровайся", "Сколько будет два плюс два?", "Напиши список идей для отпуска"])
def test_light_does_not_translate_user_wording_into_a_normative_answer_schema(monkeypatch, question):
    from proxy.services.answer_form_service import classify_answer_form, apply_response_length
    monkeypatch.setenv("LES_PRODUCT_EDITION", "light")
    form = classify_answer_form(question)
    assert form.instruction == ""
    assert "Пользователь выбрал" in apply_response_length(form, "short").instruction
