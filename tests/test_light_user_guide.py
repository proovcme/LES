from backend.user_guide_content import ARTICLES

def test_guide_discloses_actual_release_and_mail_limitations():
    assert len({article[0] for article in ARTICLES}) == len(ARTICLES)
    text = " ".join(article[3] for article in ARTICLES)
    assert "https://github.com/proovcme/LES/releases/latest" in text
    assert "не отправляет ответы" in text
    assert "OAuth требует отдельной реализации" in text


def test_public_guide_matches_embedded_help():
    from tools.light_public_docs import outputs
    for path, expected in outputs().items():
        assert path.read_text(encoding="utf-8") == expected
