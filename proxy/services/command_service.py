"""Classic compatibility codec. Command vocabulary and decisions belong to Rust."""
from backend import native_core


def is_command(question: str) -> bool:
    return (question or '').strip().startswith('/')


def list_commands() -> list[dict[str, str]]:
    return native_core.required('chat_commands', request={'action': 'list'})['value']


def handle_command(question: str, *, project_id: int | None = None) -> dict | None:
    return native_core.required('chat_commands', request={'action': 'handle', 'question': question or ''})['value']
