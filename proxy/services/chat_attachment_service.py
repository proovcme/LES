"""Classic attachment codec. Original ownership and image validation belong to Rust."""
from __future__ import annotations

import os
from pathlib import Path

from backend import native_core
from backend.runtime_paths import mutable_path

IMAGE_SUFFIXES = {'.png', '.jpg', '.jpeg', '.webp'}
DEFAULT_MAX_AGE_SEC = int(os.getenv('LES_CHAT_ATTACHMENT_MAX_AGE_SEC', str(7 * 24 * 3600)))


def _default_root():
    configured = os.getenv('LES_CHAT_ATTACHMENT_ROOT', '').strip()
    return Path(configured) if configured else mutable_path('storage/chat_attachments')


def _call(action, *, root=None, **arguments):
    try:
        return native_core.required('chat_attachments', request={
            'schema': 'les.chat-attachments.v1', 'action': action,
            'root': str((Path(root) if root is not None else _default_root()).resolve()), **arguments})['value']
    except native_core.NativeCoreError as error:
        code = str(error)
        if code in {'ATTACHMENT_NOT_FOUND', 'ATTACHMENT_EXPIRED'}:
            raise FileNotFoundError(code) from error
        messages = {'ATTACHMENT_IMAGE_LIMIT': 'Изображение больше 10 МБ',
                    'ATTACHMENT_IMAGE_PIXELS': 'Изображение больше 25 мегапикселей',
                    'ATTACHMENT_IMAGE_FORMAT': 'Поддерживаются PNG, JPEG и WebP',
                    'ATTACHMENT_IMAGE_INVALID': 'Изображение повреждено или не поддерживается'}
        raise ValueError(messages.get(code, code)) from error


def image_data_url(path):
    return _call('image', path=str(Path(path).resolve()))['data_url']


def with_image_attachment(messages, attachment_id):
    if not attachment_id:
        return messages
    _, metadata = resolve_read_attachment(attachment_id)
    if metadata['suffix'] not in IMAGE_SUFFIXES:
        return messages
    image = _call('resolve_image', attachment_id=str(attachment_id).strip().lower(),
                  max_age_sec=DEFAULT_MAX_AGE_SEC)['data_url']
    result = [dict(message) for message in messages]
    for message in reversed(result):
        if message.get('role') == 'user':
            message['content'] = [{'type': 'text', 'text': str(message.get('content') or '')},
                                  {'type': 'image_url', 'image_url': {'url': image}}]
            break
    return result


def cleanup_expired(*, root=None, max_age_sec=DEFAULT_MAX_AGE_SEC):
    return _call('cleanup', root=root, max_age_sec=max_age_sec)


def preserve_read_attachment(source, *, attachment_id, original_name, root=None):
    return _call('preserve', root=root, source=str(Path(source).resolve()),
                 attachment_id=str(attachment_id or '').strip().lower(), original_name=original_name)


def resolve_read_attachment(attachment_id, *, root=None, max_age_sec=DEFAULT_MAX_AGE_SEC):
    result = _call('resolve', root=root, attachment_id=str(attachment_id or '').strip().lower(),
                   max_age_sec=max_age_sec)
    return Path(result['path']), result['metadata']


def consume_read_attachment(attachment_id, *, root=None):
    _call('consume', root=root, attachment_id=str(attachment_id or '').strip().lower())
