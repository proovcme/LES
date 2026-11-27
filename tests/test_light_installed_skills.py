import pytest
from proxy.services import installed_skill_service as service


def test_full_text_paging_and_cross_skill_reference(tmp_path, monkeypatch):
    monkeypatch.setattr(service, 'library_root', lambda: tmp_path)
    for name in ('estimate', 'general'):
        (tmp_path / name).mkdir()
        (tmp_path / name / 'SKILL.md').write_text('description: Проверка\n' + 'Я字' * 7000, encoding='utf-8')
    assert service.catalog()['total'] == 2
    offset, parts = 0, []
    while offset is not None:
        result = service.read('estimate', '../general/SKILL.md', offset)
        parts.append(result['text'])
        offset = result['next_offset']
    assert ''.join(parts) == (tmp_path / 'general/SKILL.md').read_text(encoding='utf-8')
    assert result['arbitrary_code_execution'] is False
    for name, path in (('../escape', 'SKILL.md'), ('estimate', '../../secret.txt'),
                       ('estimate', 'C:/secret.txt'), ('estimate', 'SKILL.md:secret')):
        with pytest.raises(ValueError):
            service.read(name, path)


def test_symlink_escape_is_rejected(tmp_path, monkeypatch):
    library = tmp_path / 'library'
    library.mkdir()
    monkeypatch.setattr(service, 'library_root', lambda: library)
    package = library / 'example'
    package.mkdir()
    (package / 'SKILL.md').write_text('description: Test', encoding='utf-8')
    secret = tmp_path / 'private.md'
    secret.write_text('secret', encoding='utf-8')
    try:
        (package / 'link.md').symlink_to(secret)
    except OSError:
        import _winapi
        _winapi.CreateJunction(str(tmp_path), str(package / 'outside'))
        with pytest.raises(ValueError):
            service.read('example', 'outside/private.md')
        return
    with pytest.raises(ValueError):
        service.read('example', 'link.md')


def test_skill_page_is_not_silently_cut_between_tool_rounds():
    from proxy.services.chat_prompt_support import _compact_tool_result_for_prompt
    result = {'text': 'Инструкция ' * 500, 'next_offset': 6000}
    compacted = _compact_tool_result_for_prompt(
        {'tool': 'read_installed_skill', 'status': 'ok', 'result': result}, max_chars=2400)
    assert compacted['result'] == result
