"""Text and names survive conversion and proof links without modifying originals."""
import codecs
import json

import pytest
from backend.converter import convert_to_markdown
from backend.text_decoding import decode_document_text
from sovushka.answer_render import citation_drawer_item, source_chip


@pytest.mark.parametrize('encoding', ['utf-8', 'utf-8-sig', 'utf-16', 'utf-32'])
@pytest.mark.parametrize('suffix', ['.txt', '.md', '.json', '.jsonl'])
def test_unicode_bom_languages_spaces_and_exact_original(tmp_path, encoding, suffix):
    text = 'Лес помнит. 日本語 中文 العربية. Café e\u0301. Ёлка\u00a0и\u202fсосна.\n  Два пробела\tтабуляция 🌲'
    path = tmp_path / ('Папка with spaces & # [1]') / ('План №1 + 50% # 🌲' + suffix)
    path.parent.mkdir()
    content = json.dumps({'text': text}, ensure_ascii=False) if suffix in ('.json', '.jsonl') else text
    original = content.encode(encoding)
    path.write_bytes(original)
    converted = convert_to_markdown(path)
    assert text in converted
    assert path.read_bytes() == original
    assert list(path.parent.iterdir()) == [path]
    assert source_chip({'doc_name': str(path)})['file'] == path.name
    item = citation_drawer_item({'doc_id': 'id & + # % 中文', 'doc_name': path.name})
    assert '%23' in item['open_url'] and '%25' in item['open_url']
    assert '# %' not in item['open_url']


@pytest.mark.parametrize('encoding,text', [
    ('cp1251', 'Планируем открыть библиотеку. Ответственная Марина Лесная. Срок завершения — июнь 2033 года. ' * 4),
])
def test_legacy_encoding_preserves_every_character(encoding, text):
    assert decode_document_text(text.encode(encoding)) == text


def test_ambiguous_latin_encoding_does_not_change_french_into_hungarian_letters():
    text = 'Résumé du projet: café, coût prévu pour décembre. ' * 4
    with pytest.raises(ValueError, match='неоднозначна'):
        decode_document_text(text.encode('cp1252'))


@pytest.mark.parametrize('data', [b'\x00\x01 binary', codecs.BOM_UTF16_LE + b'\x21', b'\xff\xfe\x01'])
def test_corrupt_text_is_rejected_instead_of_silently_losing_bytes(data):
    with pytest.raises(ValueError): decode_document_text(data)


def test_utf16_csv_keeps_column_names_and_values(tmp_path):
    path = tmp_path / 'Таблица with spaces.csv'
    path.write_text('Имя,Описание\nЁлка,日本語\n', encoding='utf-16')
    result = convert_to_markdown(path)
    assert 'Имя' in result and 'Ёлка' in result and '日本語' in result


def test_pdf_images_live_in_les_state_and_same_names_do_not_collide(tmp_path, monkeypatch):
    from backend.converter import _pdf_image_dir
    state = tmp_path / 'LES state' / 'data'
    monkeypatch.setenv('RAG_META_DB_PATH', str(state / 'meta.db'))
    paths = [tmp_path / name / 'Книга #1.pdf' for name in ('Оригиналы', 'Другая папка')]
    for path in paths:
        path.parent.mkdir()
        path.write_bytes(b'%PDF-synthetic')
    destinations = [_pdf_image_dir(path) for path in paths]
    assert destinations[0] != destinations[1]
    for path, destination in zip(paths, destinations):
        assert destination.is_relative_to(state)
        (destination / 'image.png').write_bytes(b'synthetic extracted image')
        assert list(path.parent.iterdir()) == [path]
        assert path.read_bytes() == b'%PDF-synthetic'
