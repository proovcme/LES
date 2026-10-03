import pytest
from backend.imap_names import encode_mailbox, decode_mailbox
from backend.mail_ingest import _quote_imap_folder
from proxy.services.mail_sync_service import parse_imap_list_row, discover_imap_folders


@pytest.mark.parametrize('name', ['Отправленные письма', 'Проект & Архив', '日本語/中文', 'Café 🌲', 'INBOX'])
def test_unicode_mailboxes_display_human_names_but_select_exact_wire_id(name):
    wire = encode_mailbox(name)
    assert wire.isascii() and decode_mailbox(wire) == name
    parsed = parse_imap_list_row(f'(\\HasNoChildren) "/" "{wire}"'.encode('ascii'))
    assert parsed.native_id == wire and parsed.path == name
    assert _quote_imap_folder(name) == _quote_imap_folder(wire, encoded=True)
    explicit = discover_imap_folders(None, [name])[0]
    assert explicit.native_id == wire and explicit.path == name


def test_rfc_example_and_literal_ampersand():
    assert decode_mailbox('~peter/mail/&U,BTFw-/&ZeVnLIqe-') == '~peter/mail/台北/日本語'
    assert encode_mailbox('A & B') == 'A &- B'
