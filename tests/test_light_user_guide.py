from nicegui import Client
from nicegui.page import page

from sovushka.components.light_guide import ARTICLES, matching_articles, open_user_guide


def test_guide_search_is_case_insensitive_and_finds_real_topics():
    assert any(article[0] == "yandex" for article in matching_articles("ЯНДЕКС IMAP"))
    assert any(article[0] == "sources" for article in matching_articles("источник"))
    assert matching_articles("неизвестноеслово123") == []
    assert len({article[0] for article in ARTICLES}) == len(ARTICLES)


def test_guide_renders_offline_with_close_control():
    with Client(page("/__light_guide")) as client:
        dialog = open_user_guide()
        assert dialog.value
        assert any(element._props.get("aria-label") == "Закрыть руководство" for element in client.elements.values())
        assert any(getattr(element, "text", "") == "Руководство LES RAG" for element in client.elements.values())


def test_guide_discloses_actual_release_and_mail_limitations():
    text = " ".join(article[3] for article in ARTICLES)
    assert "0.1.0" in text
    assert "первый выпуск" in text
    assert "не отправляет ответы" in text
    assert "OAuth требует отдельной реализации" in text


def test_public_guide_matches_embedded_help():
    from tools.light_public_docs import outputs
    for path, expected in outputs().items():
        assert path.read_text(encoding="utf-8") == expected
