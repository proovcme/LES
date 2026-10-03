"""Read Outlook MSG into a small internal record; never follow attachments by reference."""
from dataclasses import dataclass
from email.utils import formataddr
from pathlib import Path


@dataclass(frozen=True)
class MessageAttachment:
    name: str
    data: bytes | None


@dataclass(frozen=True)
class OutlookMessage:
    subject: str
    sender: str
    to: str
    cc: str
    bcc: str
    date: str
    message_id: str
    in_reply_to: str
    references: str
    importance: str
    body: str
    attachments: tuple[MessageAttachment, ...]


def read_msg(path: str | Path) -> OutlookMessage:
    from oxmsg import Message

    message = Message.load(str(path))
    headers = {key.lower(): value for key, value in message.message_headers.items()}
    recipients = {1: [], 2: [], 3: []}
    for recipient in message.recipients:
        kind = recipient.properties.int_prop_value(0x0C15)  # PidTagRecipientType
        if kind in recipients:
            recipients[kind].append(formataddr((recipient.name, recipient.email_address)))
    body = message.body or ''
    if not body and message.html_body:
        from bs4 import BeautifulSoup
        body = BeautifulSoup(message.html_body, 'html.parser').get_text('\n', strip=True)
    return OutlookMessage(
        subject=message.subject, sender=message.sender or '',
        to=headers.get('to') or ', '.join(recipients[1]),
        cc=headers.get('cc') or ', '.join(recipients[2]),
        bcc=headers.get('bcc') or ', '.join(recipients[3]),
        date=headers.get('date') or str(message.sent_date or ''),
        message_id=headers.get('message-id') or message.properties.str_prop_value(0x1035) or '',
        in_reply_to=headers.get('in-reply-to') or message.properties.str_prop_value(0x1042) or '',
        references=headers.get('references') or message.properties.str_prop_value(0x1039) or '',
        importance=headers.get('importance', 'normal'), body=body,
        attachments=tuple(MessageAttachment(
            attachment.file_name or attachment.properties.str_prop_value(0x3704) or '(без имени)',
            attachment.file_bytes if attachment.attached_by_value else None,
        ) for attachment in message.attachments),
    )
