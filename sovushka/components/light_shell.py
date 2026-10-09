"""LES RAG workspace shell, shared by chat, datasets, mail and history.

The forest landing page is the visual reference. The inherited LES header must
not be mounted in Light: it carries unrelated runtime controls and settings.
"""
from __future__ import annotations

from nicegui import app, ui


_LIGHT_SHELL_CSS = """
.nicegui-content:has(> .light-shell){padding:0!important;gap:0!important}
.light-shell.sov-app-shell{display:grid!important;grid-template-columns:216px minmax(0,1fr);grid-template-rows:minmax(0,1fr);height:100dvh;overflow:hidden;background:#f5f8f0;color:#143b2a}
.light-shell .light-nav{grid-column:1;grid-row:1;min-width:0;display:flex;flex-direction:column;padding:27px 14px 18px;background:#fafbf6;border-right:1px solid #d7dfcf}
.light-shell .light-brand{display:flex;align-items:center;gap:12px;padding:0 10px 27px;color:#143b2a;text-decoration:none}
.light-shell .light-brand-icon{font-size:34px;color:#246c43}
.light-shell .light-brand-name{font-size:24px;line-height:1;font-weight:800;letter-spacing:.035em}
.light-shell .light-brand-name small{font-size:10px;letter-spacing:.2em;margin-left:5px}
.light-shell .light-brand-subtitle{font-size:12px;color:#536656;margin-top:6px}
.light-shell .light-nav-caption{font-size:11px;font-weight:700;letter-spacing:.17em;text-transform:uppercase;color:#536656;padding:0 13px 9px}
.light-shell .light-nav-tabs{width:100%;align-items:stretch;color:#143b2a}
.light-shell .light-nav-tabs .q-tabs__content{align-items:stretch}
.light-shell .light-nav-tabs .q-tab{justify-content:flex-start;min-height:48px;border-radius:11px;margin:2px 0;padding:0 12px;font-size:14px;text-transform:none;font-weight:650}
.light-shell .light-nav-tabs .q-tab__content{min-width:0;flex-direction:row;justify-content:flex-start;gap:12px}
.light-shell .light-nav-tabs .q-tab__label{font-size:14px;line-height:1.3}
.light-shell .light-nav-tabs .q-tab__icon{font-size:20px}
.light-shell .light-nav-tabs .q-tab--active{background:#e6eedc;color:#143b2a}
.light-shell .light-nav-tabs .q-tabs__arrow,.light-shell .light-nav-tabs .q-tab__indicator{display:none}
.light-shell .light-nav-footer{margin-top:auto;border-top:1px solid #d7dfcf;padding-top:15px}
.light-shell .light-settings-link{display:flex;align-items:center;gap:12px;min-height:46px;padding:0 13px;border-radius:11px;color:#143b2a;text-decoration:none;font-size:14px;font-weight:650}
.light-shell .light-settings-link:hover,.light-shell .light-brand:hover{background:#e6eedc}
.light-shell .light-mobile-nav{display:none}
.light-shell > .sov-app-content{grid-column:2;grid-row:1;min-height:0;min-width:0;height:100%;margin:0!important;overflow:auto}
.light-shell .sov-chat-workspace{height:100dvh}
/* The same landscape and paper surfaces as the forest screen, across real workspaces. */
.light-shell{--light-ink:#143b2a;--light-muted:#536656;--light-leaf:#246c43;--light-line:#d7dfcf;--light-paper:#fafbf6;--light-meadow:#eef2e8}
.light-shell .sov-chat-shell{gap:0;padding:0;background:var(--light-meadow);min-height:0}
.light-shell .sov-chat-main,.light-shell .sov-artifacts-panel,.light-shell .sov-history-drawer{border:0;border-radius:0;box-shadow:none;backdrop-filter:none}
.light-shell .sov-chat-main{background:radial-gradient(ellipse at 54% 66%,#fbfcf6 0%,#f2f7ed 72%);min-height:0}
.light-shell .sov-conversation-heading{padding:30px 38px 14px;align-items:flex-start}
.light-shell .sov-conversation-title{font-size:clamp(29px,2.7vw,42px);font-weight:600;letter-spacing:-.05em;line-height:1.12;color:var(--light-ink)}
.light-shell .sov-workspace-eyebrow{font-size:11px;letter-spacing:.18em;font-weight:750;color:var(--light-muted)}
.light-shell .sov-chat-topbar{padding:8px 20px;min-height:64px;background:transparent;border-bottom:1px solid var(--light-line);flex-wrap:nowrap}
.light-shell .sov-workspace-header-actions{flex-wrap:nowrap;min-width:0}
.light-shell .sov-workspace-header-actions .q-btn__content{flex-direction:row;flex-wrap:nowrap}
.light-shell .sov-scope-btn{max-width:220px;min-width:44px}
.light-shell .sov-scope-btn .block{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.light-shell .sov-chat-scroll{background:transparent}
.light-shell .sov-chat-thread{max-width:900px;padding:20px 24px!important}
.light-shell .sov-chat-empty{position:relative;isolation:isolate;box-shadow:none;background:transparent;border:0;max-width:720px;min-height:0;padding:28px 36px;text-align:left}
.light-shell .sov-chat-empty::before{content:'';position:absolute;z-index:-1;inset:-75px -32px -85px;background:url('/qdrant-visualizer/forest-mist.svg') center 40%/min(100%,640px) auto no-repeat;opacity:.12;pointer-events:none}
.light-shell .sov-chat-empty-title{font-size:clamp(33px,3.4vw,52px);font-weight:600;letter-spacing:-.055em;line-height:1.08;color:var(--light-ink)}
.light-shell .sov-chat-empty-copy{font-size:15px;line-height:1.65;color:var(--light-muted);max-width:490px;margin-top:12px}
.light-shell .sov-artifacts-panel{background:var(--light-paper);border-left:1px solid var(--light-line);padding:26px 20px}
.light-shell .sov-composer{max-width:min(900px,calc(100% - 32px))!important;background:#fff;border:1px solid var(--light-line);border-radius:18px;box-shadow:0 14px 36px #143b2a0d!important;padding:10px 14px!important;margin:0 auto 12px}
.light-shell .sov-composer-input .q-field__native{min-height:28px!important;max-height:min(160px,20dvh)}
.light-shell .sov-chat-message-text{font-size:15px;line-height:1.68}
.light-shell .chat-msg-ai,.light-shell .chat-msg-user{border-radius:16px;border:1px solid var(--light-line);box-shadow:0 4px 18px #143b2a08}
.light-shell .chat-msg-ai{background:#fff;border-left:3px solid var(--light-leaf)}
.light-shell .chat-msg-user{background:#e8f0df;border-right:3px solid var(--light-leaf)}
.light-shell .sov-datasets-page{max-width:none;padding:0;gap:0!important;grid-template-columns:minmax(0,1fr) minmax(300px,350px);grid-template-areas:'hero summary' 'registry summary' 'processing processing';background:var(--light-meadow)}
.light-shell .sov-datasets-hero{position:relative;min-height:265px;padding:36px 42px!important;background:radial-gradient(ellipse at 68% 86%,#fbfcf6 0%,#f2f7ed 70%)!important;overflow:hidden;border:0;border-bottom:1px solid var(--light-line)!important;border-radius:0!important}
.light-shell .sov-datasets-hero::after{content:'';position:absolute;right:-10px;bottom:-46px;width:min(56%,540px);height:255px;background:url('/qdrant-visualizer/forest-mist.svg') center bottom/contain no-repeat;opacity:.85;pointer-events:none}
.light-shell .sov-datasets-hero>*{position:relative;z-index:1}
.light-shell .sov-datasets-hero__row{align-items:flex-start;gap:16px}
.light-shell .sov-datasets-hero__icon{font-size:30px;color:var(--light-leaf);margin-top:4px}
.light-shell .sov-datasets-hero__title{font-size:clamp(34px,3vw,50px);font-weight:600;letter-spacing:-.055em;line-height:1.1;color:var(--light-ink)}
.light-shell .sov-datasets-hero__detail{max-width:470px;font-size:15px;line-height:1.65;color:var(--light-muted);margin-top:14px}
.light-shell .sov-dataset-add{min-height:46px;border-radius:12px;box-shadow:0 8px 20px #143b2a15}
.light-shell .sov-dataset-summary{grid-area:summary;align-self:stretch;background:var(--light-paper)!important;border:0!important;border-left:1px solid var(--light-line)!important;border-radius:0!important;box-shadow:none!important;padding:38px 28px!important;gap:26px}
.light-shell .sov-dataset-summary__title{font-size:18px;font-weight:700;color:var(--light-ink)}
.light-shell .sov-dataset-summary__detail{font-size:13px;line-height:1.55;color:var(--light-muted)}
.light-shell .sov-dataset-summary__metrics{gap:0;border-top:1px solid var(--light-line)}
.light-shell .sov-dataset-summary__metric{padding:18px 8px 18px 0!important;border-bottom:1px solid var(--light-line)!important}
.light-shell .sov-dataset-summary__value{font-size:30px;font-weight:650;line-height:1.15;color:var(--light-ink)}
.light-shell .sov-dataset-summary__label{font-size:13px;line-height:1.45;color:var(--light-muted)}
.light-shell .sov-dataset-registry-panel{grid-area:registry;min-height:400px;background:var(--light-paper)!important;border:0!important;border-radius:0!important;box-shadow:none!important;padding:32px 42px!important}
.light-shell .sov-dataset-section-head{margin-bottom:20px}
.light-shell .sov-dataset-toolbar{gap:12px;margin-bottom:18px}
.light-shell .sov-dataset-results{gap:0!important}
.light-shell .sov-dataset-row{background:transparent!important;border:0!important;border-top:1px solid var(--light-line)!important;border-radius:0!important;box-shadow:none!important;padding:23px 2px!important;gap:10px}
.light-shell .sov-dataset-row__name{font-size:17px;line-height:1.35;font-weight:700;color:var(--light-ink)}
.light-shell .sov-dataset-row__facts{gap:12px}
.light-shell .sov-dataset-row__actions{gap:10px}
.light-shell .sov-datasets-page>.sov-dataset-disclosure{grid-area:processing;margin:24px 42px 40px;width:calc(100% - 84px)!important;background:var(--light-paper)!important;border:1px solid var(--light-line);border-radius:14px}
@media(max-width:850px){
  .light-shell.sov-app-shell{grid-template-columns:minmax(0,1fr);grid-template-rows:minmax(0,1fr) 68px}
  .light-shell .light-nav{display:none}
  .light-shell > .sov-app-content{grid-column:1;grid-row:1}
  .light-shell .sov-chat-workspace{height:calc(100dvh - 68px)}
  .light-shell .light-mobile-nav{grid-column:1;grid-row:2;display:flex;align-items:stretch;justify-content:space-around;gap:2px;padding:5px 7px max(5px,env(safe-area-inset-bottom));background:#fafbf6;border-top:1px solid #d7dfcf;z-index:2}
  .light-shell .light-mobile-tab{display:flex;flex:1;flex-direction:column;align-items:center;justify-content:center;min-width:0;min-height:52px;border:0;border-radius:11px;background:transparent;color:#536656;text-decoration:none;font-size:11px;font-weight:650;line-height:1.2;text-transform:none;padding:4px 2px}
  .light-shell .light-mobile-tab .q-btn__content{display:flex;flex-direction:column;gap:2px}
  .light-shell .light-mobile-tab .q-icon{font-size:20px}
  .light-shell .light-mobile-tab--active{background:#e6eedc;color:#143b2a}
  .light-shell .sov-datasets-page{display:flex!important;flex-direction:column}
  .light-shell .sov-datasets-hero{width:100%;min-height:245px;padding:26px 22px!important}
  .light-shell .sov-datasets-hero::after{width:55%;height:185px;right:-20px;bottom:-35px;opacity:.55}
  .light-shell .sov-dataset-summary{width:100%;border-left:0!important;border-bottom:1px solid var(--light-line)!important;padding:24px 22px!important}
  .light-shell .sov-dataset-registry-panel{width:100%;min-height:0;padding:26px 22px!important}
  .light-shell .sov-datasets-page>.sov-dataset-disclosure{margin:14px 22px 28px;width:calc(100% - 44px)!important}
  .light-shell .sov-conversation-heading{padding:22px 22px 12px}
  .light-shell .sov-chat-topbar{padding:8px 12px;flex-wrap:wrap}
  .light-shell .sov-workspace-header-actions>.q-btn:not(.sov-scope-btn){width:44px;min-width:44px;padding:0!important}
  .light-shell .sov-workspace-header-actions>.q-btn:not(.sov-scope-btn) .q-btn__content>.block{display:none!important;font-size:0!important;max-width:0;overflow:hidden}
  .light-shell .sov-composer-action,.light-shell .sov-response-settings-btn{width:44px!important;min-width:44px;padding:0!important}
  .light-shell .sov-composer-action .q-btn__content>.block,.light-shell .sov-response-settings-btn .q-btn__content>.block{display:none!important;font-size:0!important;max-width:0;overflow:hidden}
  .light-shell .sov-composer-actions .q-icon{margin:0!important}
  .light-shell .sov-chat-thread{padding:16px!important}
  .light-shell .sov-chat-empty{padding:22px;min-height:0}
  .light-shell .sov-chat-empty::before{inset:-40px -12px -35px;background-size:min(100%,500px) auto}
  .light-shell .sov-composer{max-width:calc(100% - 24px)!important;margin-bottom:8px;padding:8px 12px!important}
}
@media(max-width:420px){.light-shell .light-mobile-tab{font-size:10px}.light-shell .sov-datasets-hero{min-height:230px}.light-shell .sov-datasets-hero__title{font-size:36px}.light-shell .sov-dataset-add{font-size:12px}.light-shell .sov-chat-empty-title{font-size:34px}}
@media(prefers-reduced-motion:reduce){.light-shell *{scroll-behavior:auto!important}}
"""


def build_light_shell() -> tuple[object, dict[str, object]]:
    """Render real Light destinations and return NiceGUI tabs for panel wiring."""
    from sovushka.components.navigation import return_controls

    ui.add_css(_LIGHT_SHELL_CSS)
    with ui.element("aside").classes("light-nav"):
        with ui.link(target="/qdrant-visualizer/index.html").classes("light-brand"):
            ui.icon("o_forest").classes("light-brand-icon")
            with ui.column().classes("gap-0"):
                with ui.element("span").classes("light-brand-name"):
                    ui.html("ЛЕС <small>RAG</small>", sanitize=True)
                ui.label("Ваши знания. Живой лес.").classes("light-brand-subtitle")
        ui.label("Рабочее пространство").classes("light-nav-caption")
        with ui.tabs().props("vertical no-caps align=left").classes("light-nav-tabs") as tabs:
            refs = {
                "chat": ui.tab("AI ЧАТ", label="Чат", icon="o_forum"),
                "data": ui.tab("Данные", icon="o_folder_open"),
                "mail": ui.tab("Почта", icon="o_mail_outline"),
                "history": ui.tab("История", icon="o_history"),
            }
        with ui.element("div").classes("light-nav-footer"):
            with ui.link(target="/qdrant-visualizer/index.html").classes("light-settings-link"):
                ui.icon("o_forest")
                ui.label("Лес знаний")
            with ui.link(target="/les/classic?tab=models").classes("light-settings-link"):
                ui.icon("o_tune")
                ui.label("Настройки")
            return_controls()

    mobile_buttons = {}
    with ui.element("nav").classes("light-mobile-nav"):
        for key, label, icon in (
            ("chat", "Чат", "o_forum"),
            ("data", "Данные", "o_folder_open"),
            ("mail", "Почта", "o_mail_outline"),
            ("history", "История", "o_history"),
        ):
            mobile_buttons[key] = ui.button(
                label, icon=icon, on_click=lambda key=key: tabs.set_value(refs[key]),
            ).props("flat no-caps").classes("light-mobile-tab")
        with ui.button("Ещё", icon="o_more_horiz").props('flat no-caps aria-label="Ещё разделы"').classes("light-mobile-tab"):
            with ui.menu().props("auto-close").classes("sov-chat-utility-menu"):
                return_controls()
                ui.menu_item("Лес знаний", on_click=lambda: ui.navigate.to("/qdrant-visualizer/index.html"))
                ui.menu_item("Настройки", on_click=lambda: ui.navigate.to("/les/classic?tab=models"))

    def remember_tab(event) -> None:
        app.storage.user["last_chat_tab"] = str(event.value or "AI ЧАТ")
        for key, button in mobile_buttons.items():
            button.classes(
                add="light-mobile-tab--active" if event.value == refs[key]._props.get("name") else "",
                remove="" if event.value == refs[key]._props.get("name") else "light-mobile-tab--active",
            )
            if event.value == refs[key]._props.get("name"):
                button.props('aria-current="page"')
            else:
                button.props(remove="aria-current")

    tabs.on_value_change(remember_tab)
    return tabs, refs
