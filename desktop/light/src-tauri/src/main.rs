#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{fs, io::{Read, Write}, net::{TcpStream, SocketAddr}, path::PathBuf,
    process::{Child, Command, Stdio}, sync::{Mutex, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};
use tauri::Manager;
use tauri::{menu::{Menu, MenuItem}, tray::TrayIconBuilder};
use serde_json::{json, Value};
mod close_guard;
mod lifecycle;
mod supervisor;
mod launcher;
mod update;
#[path = "../../../../runtime/src/desktop_control.rs"]
mod desktop_control;
#[cfg(windows)] use std::os::windows::process::CommandExt;

struct Runtime { directory: PathBuf, state: PathBuf, child: Mutex<Option<Child>>, close: Mutex<close_guard::CloseGuard>,
                 stopping: AtomicBool, launch_id: Option<String>, update_operation: AtomicBool,
                 pending_update: Mutex<Option<Value>> }

fn read_status(runtime: &Runtime) -> Value {
    let path = runtime.state.join("launcher-status.json");
    if fs::metadata(&path).map(|m| m.len() > 65536).unwrap_or(false) {
        return json!({"phase":"error","message":"Не удалось прочитать состояние. Откройте журнал запуска."});
    }
    fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(json!({"phase":"starting","message":"Подготавливаю LES RAG…"}))
}

fn owned_url(status: &Value) -> Option<tauri::Url> {
    let url = tauri::Url::parse(status.get("ui_url")?.as_str()?).ok()?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port().is_none()
        || url.path() != "/app/" || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some() { return None; }
    Some(url)
}

fn instance_ready(status: &Value) -> bool {
    let Some(url) = owned_url(status) else { return false; };
    let Some(expected) = status.get("instance_id").and_then(Value::as_str) else { return false; };
    let address = SocketAddr::from(([127,0,0,1], url.port().unwrap()));
    let Ok(mut socket) = TcpStream::connect_timeout(&address, Duration::from_millis(600)) else { return false; };
    let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = socket.set_write_timeout(Some(Duration::from_secs(1)));
    let request = b"GET /api/light/instance HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    if socket.write_all(request).is_err() { return false; }
    let mut response = String::new();
    if socket.take(65536).read_to_string(&mut response).is_err() { return false; }
    let Some((headers, body)) = response.split_once("\r\n\r\n") else { return false; };
    if !headers.lines().next().unwrap_or("").contains(" 200 ") { return false; }
    serde_json::from_str::<Value>(body).ok().and_then(|v| v.get("instance_id").and_then(Value::as_str).map(|s| s == expected)).unwrap_or(false)
}

fn stop_child(runtime: &Runtime, reason: &str) {
    if let Ok(mut guard) = runtime.child.lock() {
        if let Some(mut child) = guard.take() {
            lifecycle::event(&runtime.state,"desktop","launcher_stop_requested",json!({"child_pid":child.id(),"reason":reason}));
            if let Some(mut input) = child.stdin.take() { let _ = input.write_all(b"stop\n"); }
            let deadline = Instant::now() + Duration::from_secs(12);
            while Instant::now() < deadline {
                if let Some(code) = child.try_wait().ok().flatten() {
                    lifecycle::event(&runtime.state,"desktop","launcher_stopped",json!({"child_pid":child.id(),"exit_code":code.code(),"reason":reason}));
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
            lifecycle::event(&runtime.state,"desktop","launcher_forced_stop",json!({"child_pid":child.id(),"reason":"shutdown_timeout","requested_reason":reason}));
            let _ = child.kill(); // owned launcher; its Windows job stops its descendants
            let _ = child.wait();
        }
    }
}

fn start_child(runtime: &Runtime) -> Result<(), String> {
    let mut owned = runtime.child.lock().map_err(|_| "Запуск занят")?;
    if runtime.stopping.load(Ordering::SeqCst) { return Err("Приложение закрывается".into()); }
    if owned.as_mut().is_some_and(|child| child.try_wait().ok().flatten().is_none()) { return Err("Компоненты уже запущены".into()); }
    let root = runtime.directory.join("runtime");
    fs::create_dir_all(runtime.state.join("logs")).map_err(|_| "Нет доступа к папке данных LES RAG. Проверьте права пользователя.")?;
    let output = fs::OpenOptions::new().create(true).append(true).open(runtime.state.join("logs/launcher.txt")).map_err(|_| "Не удалось открыть журнал запуска.")?;
    let mut command = launcher::command(&runtime.directory, &root, &runtime.state).map_err(str::to_string)?;
    command.current_dir(&runtime.state).stdin(Stdio::piped()).stdout(Stdio::from(output.try_clone().map_err(|e| e.to_string())?)).stderr(Stdio::from(output));
    if std::env::args().any(|arg| arg == "--read-only") { command.arg("--read-only"); }
    #[cfg(windows)] command.creation_flags(0x08000000);
    let child = command.spawn().map_err(|_| "Windows не смог запустить компоненты LES RAG. Проверьте защитное ПО или восстановите установку.")?;
    lifecycle::event(&runtime.state,"desktop","launcher_started",json!({"child_pid":child.id()}));
    *owned = Some(child);
    Ok(())
}

#[tauri::command]
fn startup_status(runtime: tauri::State<Runtime>) -> Value { read_status(&runtime) }

fn validate_editor(window: &tauri::WebviewWindow, runtime: &Runtime) -> Result<(), String> {
    let actual = window.url().map_err(|_| "Не удалось проверить окно")?;
    let status = read_status(runtime);
    let expected = status.get("api_url").and_then(Value::as_str).and_then(close_guard::editor_url).ok_or("Приложение не готово")?;
    if window.label() != "main" || !close_guard::owned_editor(&actual, &expected) {
        return Err("Подтверждение разрешено только текущему чату Леса".into());
    }
    Ok(())
}

#[tauri::command]
async fn pick_folder(window: tauri::WebviewWindow, runtime: tauri::State<'_, Runtime>, initial: Option<String>) -> Result<Option<String>, String> {
    validate_editor(&window, &runtime)?;
    #[cfg(windows)]
    {
        let owner = window.hwnd().map_err(|_| "Не удалось найти окно Леса.")?.0 as isize;
        tauri::async_runtime::spawn_blocking(move || launcher::pick_folder(owner, initial))
            .await.map_err(|_| "Не удалось открыть выбор папки".to_string())?
    }
    #[cfg(not(windows))]
    { let _ = initial; Err("Укажите путь к папке.".into()) }
}

#[tauri::command]
fn editor_ready(window: tauri::WebviewWindow, runtime: tauri::State<Runtime>) -> Result<(), String> {
    validate_editor(&window, &runtime)?;
    runtime.close.lock().map_err(|_| "Закрытие занято")?.register();
    let _ = fs::write(runtime.state.join("editor-close-status.json"), json!({"phase":"registered","instance_id":read_status(&runtime)["instance_id"]}).to_string());
    Ok(())
}

#[tauri::command]
fn editor_close_result(window: tauri::WebviewWindow, runtime: tauri::State<Runtime>, nonce: u64, saved: bool) -> Result<(), String> {
    validate_editor(&window, &runtime)?;
    let exit = runtime.close.lock().map_err(|_| "Закрытие занято")?.acknowledge(nonce, saved)?;
    let update = runtime.pending_update.lock().map_err(|_| "Обновление занято")?.take();
    if let Some(update) = update {
        if exit { update::arm(&runtime.state, &update)?; }
        else { update::discard(&runtime.state, &update); }
    }
    let _ = fs::write(runtime.state.join("editor-close-status.json"), json!({"phase":if saved {"saved"} else {"blocked"}}).to_string());
    if !saved { let _ = window.set_title("LES RAG"); }
    if exit { window.app_handle().exit(0); }
    Ok(())
}

#[tauri::command]
fn retry_start(runtime: tauri::State<Runtime>) -> Result<(), String> {
    stop_child(&runtime, "user_retry");
    start_child(&runtime)
}

#[tauri::command]
fn open_browser(runtime: tauri::State<Runtime>) -> Result<(), String> {
    let status = read_status(&runtime);
    if !instance_ready(&status) { return Err("Приложение ещё не готово. Дождитесь запуска.".into()); }
    let url = owned_url(&status).ok_or("Адрес приложения недоступен")?;
    let mut command = Command::new("rundll32.exe");
    command.args(["url.dll,FileProtocolHandler", url.as_str()]);
    #[cfg(windows)] command.creation_flags(0x08000000);
    command.spawn().map_err(|_| "Не удалось открыть браузер".to_string())?;
    Ok(())
}

fn open_logs(runtime: &Runtime) {
    let mut command = Command::new("explorer.exe");
    command.arg(runtime.state.join("logs"));
    #[cfg(windows)] command.creation_flags(0x08000000);
    let _ = command.spawn();
}

fn request_exit(app: &tauri::AppHandle) {
    let runtime = app.state::<Runtime>();
    if let Some(webview) = app.get_webview_window("main") {
        let nonce = runtime.close.lock().ok().and_then(|mut guard| guard.request());
        if let Some(nonce) = nonce {
            let _ = fs::write(runtime.state.join("editor-close-status.json"), json!({"phase":"requested","nonce":nonce}).to_string());
            let _ = webview.set_title("Лес — сохраняю диалог…");
            let script = format!("window.dispatchEvent(new CustomEvent('les:close-request', {{detail:{{nonce:{nonce}}}}}))");
            if webview.eval(script).is_err() {
                let _ = webview.set_title("Лес — не удалось подтвердить сохранение");
                if let Ok(mut guard) = runtime.close.lock() { let _ = guard.acknowledge(nonce, false); }
                if let Ok(mut pending) = runtime.pending_update.lock() {
                    if let Some(update) = pending.take() { update::discard(&runtime.state, &update); }
                }
                let _ = fs::write(runtime.state.join("editor-close-status.json"), json!({"phase":"blocked"}).to_string());
            }
            return;
        }
    }
    app.exit(0);
}

fn explicit_state(args: &[String]) -> Result<Option<PathBuf>, String> {
    let positions: Vec<usize> = args.iter().enumerate().filter_map(|(index, value)| (value == "--state").then_some(index)).collect();
    if positions.len() > 1 { return Err("Папка состояния указана несколько раз".into()); }
    let Some(index) = positions.first() else { return Ok(None); };
    let path = args.get(index + 1).filter(|value| !value.starts_with("--")).map(PathBuf::from)
        .ok_or("После --state нужен полный путь к отдельной папке состояния")?;
    if !path.is_absolute() { return Err("Путь состояния должен быть абсолютным".into()); }
    Ok(Some(path))
}

fn main() {
    let directory = std::env::current_exe().expect("executable path").parent().unwrap().to_path_buf();
    // An explicit separate state makes candidate acceptance possible without
    // opening or copying the user's documents, models or ongoing conversation.
    let args: Vec<String> = std::env::args().collect();
    let candidate_state = match explicit_state(&args) {
        Ok(value) => value,
        Err(message) => { eprintln!("{message}"); return; }
    };
    let state = match candidate_state.or_else(|| std::env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("LES Light"))) {
        Some(path) => path,
        None => { eprintln!("Windows не сообщил папку локальных данных пользователя"); return; }
    };
    if fs::create_dir_all(&state).is_err() { eprintln!("Нет доступа к папке данных LES RAG"); return; }
    let panic_state=state.clone();
    std::panic::set_hook(Box::new(move |info| {
        lifecycle::event(&panic_state,"desktop","panic",json!({"reason":"rust_panic",
            "location":info.location().map(|at| format!("{}:{}",at.file(),at.line()))}));
    }));
    #[cfg(windows)]
    if !args.iter().any(|arg| arg == "--supervised-child") {
        let executable = std::env::current_exe().expect("executable path");
        if let Err(error) = supervisor::run(&executable, &state, &args) {
            lifecycle::event(&state,"supervisor","startup_failed",json!({"reason":error}));
            lifecycle::status(&state,"launcher-status.json",json!({"schema":"les.light-launcher.v1","phase":"error","message":error,"reason":"supervisor_startup_failed","updated_at":lifecycle::stamp()}));
        }
        return;
    }
    let launch_id = if args.iter().any(|arg| arg == "--supervised-child") {
        match supervisor::child_handshake(&args) { Ok(id) => Some(id), Err(_) => return }
    } else { None };
    let _ = fs::write(state.join("desktop-status.json"), json!({"phase":"opening_window"}).to_string());
    tauri::Builder::default()
        .manage(Runtime { directory, state, child: Mutex::new(None), close: Mutex::new(close_guard::CloseGuard::default()),
                          stopping: AtomicBool::new(false), launch_id, update_operation: AtomicBool::new(false),
                          pending_update: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![startup_status, retry_start, open_browser, editor_ready, editor_close_result, pick_folder, update::update_check, update::update_install])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                request_exit(window.app_handle());
            }
        })
        .on_page_load(|webview, payload| {
            let url = payload.url();
            if payload.event() == tauri::webview::PageLoadEvent::Started
                && (url.host_str() == Some("tauri.localhost") || matches!(url.path(),
                    "/app/" | "/qdrant-visualizer/index.html")) {
                if let Ok(mut guard) = webview.state::<Runtime>().close.lock() { guard.invalidate(); }
            }
            if payload.event() == tauri::webview::PageLoadEvent::Finished && url.host_str() == Some("127.0.0.1") && url.path() == "/app/" {
                let runtime = webview.state::<Runtime>();
                let _ = fs::write(runtime.state.join("desktop-status.json"), json!({"phase":"loaded","url":url.as_str()}).to_string());
            }
        })
        .setup(|app| {
            let _ = fs::write(app.state::<Runtime>().state.join("desktop-status.json"), json!({"phase":"creating_menu"}).to_string());
            let browser = MenuItem::with_id(app, "browser", "Открыть в браузере", true, None::<&str>)?;
            let logs = MenuItem::with_id(app, "logs", "Журнал запуска", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Закрыть LES RAG", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&browser, &logs, &quit])?;
            let mut tray = TrayIconBuilder::new().tooltip("LES RAG").menu(&menu).on_menu_event(|app, event| {
                match event.id.as_ref() {
                    "browser" => { let _ = open_browser(app.state::<Runtime>()); },
                    "logs" => open_logs(&app.state::<Runtime>()),
                    "quit" => request_exit(app),
                    _ => {}
                }
            });
            if let Some(icon) = app.default_window_icon() { tray = tray.icon(icon.clone()); }
            tray.build(app)?;
            let _ = fs::write(app.state::<Runtime>().state.join("desktop-status.json"), json!({"phase":"starting_services"}).to_string());
            let runtime = app.state::<Runtime>();
            if let Err(message) = start_child(&runtime) {
                fs::create_dir_all(&runtime.state)?;
                fs::write(runtime.state.join("launcher-status.json"), json!({"phase":"error","message":message}).to_string())?;
            }
            let handle = app.handle().clone();
            let window = app.get_webview_window("main").unwrap();
            if !std::env::args().any(|arg| arg == "--smoke-hidden") { window.show()?; }
            // Querying WebView URL synchronously in setup can wait on the event
            // loop that has not started yet on Windows.
            let initial_url = tauri::Url::parse("http://tauri.localhost/index.html")?;
            let stop_marker = runtime.launch_id.as_deref().and_then(|id| desktop_control::stop_marker(&runtime.state, id));
            thread::spawn(move || {
                let mut displayed = String::new();
                let mut recovery = lifecycle::Recovery::default();
                loop {
                    thread::sleep(Duration::from_millis(800));
                    let runtime = handle.state::<Runtime>();
                    if runtime.stopping.load(Ordering::SeqCst) { break; }
                    if stop_marker.as_ref().is_some_and(|path| fs::remove_file(path).is_ok()) {
                        let closer = handle.clone();
                        let _ = handle.run_on_main_thread(move || request_exit(&closer));
                    }
                    let Some(window) = handle.get_webview_window("main") else { continue; };
                    let mut status = read_status(&runtime);
                    let exit = runtime.child.lock().ok().and_then(|mut guard| {
                        let code = guard.as_mut()?.try_wait().ok().flatten()?;
                        guard.take(); Some(code)
                    });
                    if let Some(code) = exit {
                        lifecycle::event(&runtime.state,"desktop","launcher_exited",json!({"exit_code":code.code(),"reason":"unexpected_exit"}));
                        let delay = if code.code() == Some(10) { None } else { recovery.next(Instant::now()) };
                        if let Some(delay) = delay {
                            lifecycle::status(&runtime.state,"launcher-status.json",json!({"schema":"les.light-launcher.v1","phase":"recovering","reason":"launcher_exit","exit_code":code.code(),"message":"Компонент запуска остановился. Восстанавливаю Лес…","updated_at":lifecycle::stamp()}));
                            let _=window.navigate(initial_url.clone()); displayed.clear();
                            thread::sleep(delay);
                            if let Err(error) = start_child(&runtime) {
                                lifecycle::event(&runtime.state,"desktop","launcher_restart_failed",json!({"reason":error}));
                                lifecycle::status(&runtime.state,"launcher-status.json",json!({"schema":"les.light-launcher.v1","phase":"error","message":error,"updated_at":lifecycle::stamp()}));
                            }
                            continue;
                        }
                        status=json!({"schema":"les.light-launcher.v1","phase":"error","reason":if code.code()==Some(10) {"state_already_owned"} else {"recovery_limit"},"exit_code":code.code(),"message":"Автовосстановление приостановлено: повторные остановки или папка занята другим Лесом. Откройте журнал запуска.","updated_at":lifecycle::stamp()});
                        lifecycle::status(&runtime.state,"launcher-status.json",status.clone());
                        lifecycle::event(&runtime.state,"desktop","recovery_stopped",json!({"reason":status["reason"]}));
                    }
                    let id = status.get("instance_id").and_then(Value::as_str).unwrap_or("");
                    if status["phase"] == "ready" && id != displayed && instance_ready(&status) {
                        if let Some(url) = owned_url(&status) { if window.navigate(url).is_ok() { displayed = id.to_string(); } }
                    } else if matches!(status["phase"].as_str(), Some("error" | "recovering")) && !displayed.is_empty() {
                        let _ = window.navigate(initial_url.clone());
                        displayed.clear();
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("LES RAG: не удалось открыть окно")
        .run(|app, event| { if matches!(event, tauri::RunEvent::Exit) {
            let runtime=app.state::<Runtime>(); runtime.stopping.store(true,Ordering::SeqCst);
            if let Some(id)=&runtime.launch_id { supervisor::exit_intent(&runtime.state,id); }
            stop_child(&runtime,"normal_exit");
        } });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_is_limited_to_the_native_loopback_renderer() {
        assert!(owned_url(&json!({"ui_url":"http://127.0.0.1:54321/app/"})).is_some());
        for value in ["http://127.0.0.1:54321/classic", "https://example.org/app/", "http://127.0.0.1/app/", "http://user@127.0.0.1:54321/app/", "http://127.0.0.1:54321/app/?url=other", "http://127.0.0.1:54321/api/private", "http://localhost:54321/app/"] {
            assert!(owned_url(&json!({"ui_url":value})).is_none());
        }
    }
    #[test]
    fn bad_candidate_argument_never_falls_back_to_user_state() {
        for args in [vec!["les", "--state"], vec!["les", "--state", "--svelte"], vec!["les", "--state", "relative"], vec!["les", "--state", "x", "--state", "y"]] {
            assert!(explicit_state(&args.iter().map(|value| value.to_string()).collect::<Vec<_>>()).is_err());
        }
        assert!(explicit_state(&["les".into(), "--svelte".into()]).unwrap().is_none());
    }
}
