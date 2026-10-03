"""Open an answer's matching evidence drawer with one mouse or keyboard action."""
from nicegui import ui


SOURCE_LINK_CLICK = """event => {
    const link = event.target.closest('a[href^="#source-"]');
    if (!link) return;
    const target = document.getElementById(link.getAttribute('href').slice(1));
    if (!target || !target.classList.contains('sov-ui-source-chip')) return;
    event.preventDefault();
    target.click();
}"""


def source_markdown(text: str):
    return ui.markdown(text).on('click', js_handler=SOURCE_LINK_CLICK)
