"""Load the canonical UI kit in explicit cascade order, once per process."""
from pathlib import Path

_STYLES = Path(__file__).with_name("styles")
_STYLESHEETS = (
    '00_base.css',
    '01_chat.css',
    '02_tools.css',
    '03_settings.css',
    '04_lists.css',
    '05_documents.css',
    '06_workspace.css',
    '07_forest.css',
)
UIKIT_CSS = '<style id="sovushka-uikit">' + ''.join(
    (_STYLES / name).read_text(encoding="utf-8") for name in _STYLESHEETS
) + '</style>'
