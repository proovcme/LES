import json
from tools import light_cli as cli


def test_cli_search_preserves_unicode_and_uses_same_mcp_search(monkeypatch, capsys):
    calls = []
    monkeypatch.setattr(cli.gateway, 'search_sources', lambda query, datasets: calls.append((query, datasets)) or {'text': 'Лес'})
    assert cli.main(['search', 'Где файл #1 日本語?', '--dataset', 'id']) == 0
    assert calls == [('Где файл #1 日本語?', ['id'])]
    assert json.loads(capsys.readouterr().out) == {'text': 'Лес'}


def test_cli_ask_keeps_explicit_chat_and_dataset(monkeypatch, capsys):
    calls = []
    monkeypatch.setattr(cli.gateway, '_request', lambda *a, **kw: calls.append((a, kw)) or {'answer': 'Ответ'})
    assert cli.main(['ask', 'Мой вопрос', '--chat', 'chat-id', '--dataset', 'docs']) == 0
    assert len(calls) == 1
    assert calls[0][1]['body'] == {'session_id': 'chat-id', 'question': 'Мой вопрос', 'dataset_ids': ['docs'], 'mode': 'search'}
    assert json.loads(capsys.readouterr().out)['session_id'] == 'chat-id'


def test_cli_reports_unavailable_app_without_starting_another_instance(monkeypatch, capsys):
    def fail(*a, **kw): raise RuntimeError('Откройте ЛЕС')
    monkeypatch.setattr(cli.gateway, '_request', fail)
    assert cli.main(['status']) == 1
    assert 'Откройте ЛЕС' in capsys.readouterr().err
