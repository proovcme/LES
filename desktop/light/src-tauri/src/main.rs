#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{fs, io::{Read, Write}, net::{TcpStream, SocketAddr}, path::PathBuf,
    process::{Child, Command, Stdio}, sync::Mutex, thread, time::{Duration, Instant}};
use tauri::Manager;
use tauri::{menu::{Menu, MenuItem}, tray::TrayIconBuilder};
use serde_json::{json, Value};
#[cfg(windows)] use std::os::windows::process::CommandExt;

struct Runtime { directory: PathBuf, state: PathBuf, child: Mutex<Option<Child>> }

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
        || url.path() != "/classic" || !url.username().is_empty() || url.password().is_some()
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
    if socket.write_all(b"GET /healthz HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").is_err() { return false; }
    let mut response = String::new();
    if socket.take(65536).read_to_string(&mut response).is_err() { return false; }
    let Some((headers, body)) = response.split_once("\r\n\r\n") else { return false; };
    if !headers.lines().next().unwrap_or("").contains(" 200 ") { return false; }
    serde_json::from_str::<Value>(body).ok().and_then(|v| v.get("instance_id").and_then(Value::as_str).map(|s| s == expected)).unwrap_or(false)
}

fn stop_child(runtime: &Runtime) {
    if let Ok(mut guard) = runtime.child.lock() {
        if let Some(mut child) = guard.take() {
            if let Some(mut input) = child.stdin.take() { let _ = input.write_all(b"stop\n"); }
            let deadline = Instant::now() + Duration::from_secs(12);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() { return; }
                thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill(); // owned launcher; its Windows job stops its descendants
            let _ = child.wait();
        }
    }
}

fn start_child(runtime: &Runtime) -> Result<(), String> {
    let python = runtime.directory.join("python/python.exe");
    let root = runtime.directory.join("runtime");
    let launcher = root.join("tools/light_launcher.py");
    if !python.is_file() || !launcher.is_file() {
        return Err("В установке не хватает компонентов запуска. Восстановите LES RAG установщиком; ваши документы сохранятся.".into());
    }
    fs::create_dir_all(runtime.state.join("logs")).map_err(|_| "Нет доступа к папке данных LES RAG. Проверьте права пользователя.")?;
    let output = fs::OpenOptions::new().create(true).append(true).open(runtime.state.join("logs/launcher.txt")).map_err(|_| "Не удалось открыть журнал запуска.")?;
    let mut command = Command::new(python);
    command.arg("-B").arg(launcher).args(["--root"]).arg(&root).arg("--state").arg(&runtime.state)
        .arg("--qdrant").arg(runtime.directory.join("native/qdrant/qdrant.exe")).arg("--parent-pipe")
        .current_dir(&runtime.state).stdin(Stdio::piped()).stdout(Stdio::from(output.try_clone().map_err(|e| e.to_string())?)).stderr(Stdio::from(output));
    #[cfg(windows)] command.creation_flags(0x08000000);
    let child = command.spawn().map_err(|_| "Windows не смог запустить компоненты LES RAG. Проверьте защитное ПО или восстановите установку.")?;
    *runtime.child.lock().map_err(|_| "Запуск занят")? = Some(child);
    Ok(())
}

#[tauri::command]
fn startup_status(runtime: tauri::State<Runtime>) -> Value { read_status(&runtime) }

#[tauri::command]
fn retry_start(runtime: tauri::State<Runtime>) -> Result<(), String> {
    stop_child(&runtime);
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

fn main() {
    let directory = std::env::current_exe().expect("executable path").parent().unwrap().to_path_buf();
    let state = match std::env::var_os("LOCALAPPDATA") {
        Some(base) => PathBuf::from(base).join("LES Light"),
        None => { eprintln!("Windows не сообщил папку локальных данных пользователя"); return; }
    };
    if fs::create_dir_all(&state).is_err() { eprintln!("Нет доступа к папке данных LES RAG"); return; }
    let _ = fs::write(state.join("desktop-status.json"), json!({"phase":"opening_window"}).to_string());
    tauri::Builder::default()
        .manage(Runtime { directory, state, child: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![startup_status, retry_start, open_browser])
        .on_page_load(|webview, payload| {
            let url = payload.url();
            if payload.event() == tauri::webview::PageLoadEvent::Finished && url.host_str() == Some("127.0.0.1") && matches!(url.path(), "/classic" | "/les/classic") {
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
                    "quit" => app.exit(0),
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
            thread::spawn(move || {
                let mut displayed = String::new();
                loop {
                    thread::sleep(Duration::from_millis(800));
                    let Some(window) = handle.get_webview_window("main") else { break; };
                    let runtime = handle.state::<Runtime>();
                    let mut status = read_status(&runtime);
                    let exit = runtime.child.lock().ok().and_then(|mut guard| guard.as_mut().and_then(|child| child.try_wait().ok().flatten()));
                    if exit.is_some_and(|code| code.code() != Some(10)) && status["phase"] != "error" {
                        status = json!({"phase":"error","message":"Компонент запуска остановился. Повторите запуск; журнал доступен через значок LES RAG рядом с часами."});
                        let _ = fs::write(runtime.state.join("launcher-status.json"), status.to_string());
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
        .run(|app, event| { if matches!(event, tauri::RunEvent::Exit) { stop_child(&app.state::<Runtime>()); } });
}
