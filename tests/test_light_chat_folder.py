import pytest
from proxy.services.chat_folder_service import read_folder
from proxy.services.chat_attachment_service import resolve_read_attachment


def test_folder_snapshot_preserves_names_and_originals_without_index(tmp_path, monkeypatch):
    monkeypatch.setenv('LES_WINDOWS_STATE_ROOT', str(tmp_path / 'state'))
    root = tmp_path / 'Папка #1 🌲'; root.mkdir()
    source = root / 'План & 中文.txt'
    source.write_text('Сохраняем точные слова. Планируем завершить в июне 2033.', encoding='utf-16')
    before = source.read_bytes()
    result = read_folder(root)
    assert result['file_count'] == 1 and result['mode'] == 'read'
    assert source.name in result['text'] and 'Планируем' in result['text']
    assert source.read_bytes() == before and list(root.iterdir()) == [source]
    snapshot, _ = resolve_read_attachment(result['attachment_id'])
    assert snapshot.read_text(encoding='utf-8') == result['text']
    assert not list((tmp_path / 'state').rglob('*.db'))


@pytest.mark.parametrize('case', ['many', 'large', 'unreadable'])
def test_folder_does_not_silently_send_partial_content(tmp_path, monkeypatch, case):
    monkeypatch.setenv('LES_WINDOWS_STATE_ROOT', str(tmp_path / 'state'))
    root = tmp_path / 'sources'; root.mkdir()
    if case == 'many':
        for i in range(26): (root / f'{i}.txt').write_text('Документ', encoding='utf-8')
    elif case == 'large': (root / 'large.txt').write_text('x' * 48001)
    else: (root / 'bad.txt').write_bytes(b'\x00\x01binary')
    with pytest.raises(ValueError): read_folder(root)
    assert not (tmp_path / 'state/storage/chat_attachments').exists()
