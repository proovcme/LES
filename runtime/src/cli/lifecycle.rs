//! Local application lifecycle. Wait for the actual owner; never terminate a PID.
use super::gateway::{state_root, Api, Result};
use crate::desktop_control::{stop_marker, PROTOCOL};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn read(path: &Path) -> Option<Value> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(65_537)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 65_536 {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn online() -> bool {
    Api::connect()
        .and_then(|api| api.get("/api/light/instance"))
        .is_ok()
}

pub fn status() -> Result<Value> {
    if let Ok(api) = Api::connect() {
        return api.get("/api/status");
    }
    let state = state_root()?;
    let launcher = read(&state.join("launcher-status.json")).unwrap_or_default();
    let supervisor = read(&state.join("supervisor-status.json")).unwrap_or_default();
    let phase = match launcher["phase"].as_str() {
        Some(value @ ("starting" | "recovering" | "stopping" | "error" | "stopped")) => value,
        _ => "stopped",
    };
    Ok(
        json!({"status":phase,"reason":launcher["reason"],"terminal_control":supervisor["control"]==PROTOCOL}),
    )
}

#[cfg(windows)]
pub fn start() -> Result<Value> {
    use std::process::{Command, Stdio};
    if online() {
        return Ok(json!({"status":"ready","already_running":true}));
    }
    let state = state_root()?;
    let runtime = std::env::current_exe().map_err(|_| "Не удалось найти установленный Лес.")?;
    let root = runtime
        .ancestors()
        .nth(3)
        .ok_or("Не удалось найти установленный Лес.")?;
    let desktop = root.join("les-light.exe");
    if !desktop.is_file() {
        return Err("Команду start нужно запустить из установленной папки Леса.".into());
    }
    // The caller may capture stdout and stderr. Its original pipe handles must
    // not remain open in the desktop supervisor after this command returns.
    let stdio = NoInheritedStdio::new()?;
    let mut child = Command::new(desktop)
        .args(["--native-api", "--svelte", "--state"])
        .arg(&state)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Windows не смог запустить Лес.")?;
    drop(stdio);
    let deadline = Instant::now() + Duration::from_secs(85);
    while Instant::now() < deadline {
        if online() {
            return Ok(json!({"status":"ready"}));
        }
        if child.try_wait().is_ok_and(|exit| exit.is_some()) {
            // A competing start can win the desktop mutex before publishing readiness.
            let owner = read(&state.join("supervisor-status.json")).unwrap_or_default();
            let launcher = read(&state.join("launcher-status.json")).unwrap_or_default();
            if owner["phase"] != "running" && launcher["phase"] == "error" {
                return Err("Лес не запустился. Причина доступна в les logs.".into());
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err("Лес не подтвердил запуск. Проверьте les status и les logs.".into())
}

#[cfg(windows)]
struct NoInheritedStdio(Vec<*mut std::ffi::c_void>);

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(number: u32) -> *mut std::ffi::c_void;
    fn GetHandleInformation(handle: *mut std::ffi::c_void, flags: *mut u32) -> i32;
    fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
}

#[cfg(windows)]
impl NoInheritedStdio {
    const INHERIT: u32 = 1;

    fn new() -> Result<Self> {
        let mut guard = Self(Vec::with_capacity(3));
        for number in [-10i32, -11, -12] {
            let handle = unsafe { GetStdHandle(number as u32) };
            let mut flags = 0;
            if unsafe { GetHandleInformation(handle, &mut flags) } != 0
                && flags & Self::INHERIT != 0
            {
                if unsafe { SetHandleInformation(handle, Self::INHERIT, 0) } == 0 {
                    return Err("Не удалось отделить вывод терминала от окна Леса.".into());
                }
                guard.0.push(handle);
            }
        }
        Ok(guard)
    }
}

#[cfg(windows)]
impl Drop for NoInheritedStdio {
    fn drop(&mut self) {
        for handle in &self.0 {
            let _ = unsafe { SetHandleInformation(*handle, Self::INHERIT, Self::INHERIT) };
        }
    }
}

#[cfg(not(windows))]
pub fn start() -> Result<Value> {
    Err("Управление запуском настольного Леса из CLI поддерживается в Windows.".into())
}

pub fn stop() -> Result<Value> {
    let state = state_root()?;
    let supervisor_path = state.join("supervisor-status.json");
    let supervisor = read(&supervisor_path).unwrap_or_default();
    if supervisor["phase"] != "running" {
        return if online() {
            Err(
                "Этот экземпляр запущен без настольного управления. Закройте его способом запуска."
                    .into(),
            )
        } else {
            Ok(json!({"status":"stopped","already_stopped":true}))
        };
    }
    if supervisor["control"] != PROTOCOL {
        return Err(
            "Эта версия Леса ещё не поддерживает les stop. В трее выберите «Закрыть LES RAG»."
                .into(),
        );
    }
    let launch = supervisor["launch_id"]
        .as_str()
        .ok_or("Не удалось определить окно Леса.")?;
    let marker = stop_marker(&state, launch).ok_or("Не удалось определить окно Леса.")?;
    let close_path = state.join("editor-close-status.json");
    let before = fs::metadata(&close_path).and_then(|m| m.modified()).ok();
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("Не удалось передать команду остановки в папку Леса.".into()),
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let status = read(&supervisor_path).unwrap_or_default();
        if status["phase"] == "stopped" && status["launch_id"] == launch {
            return Ok(json!({"status":"stopped"}));
        }
        if status["phase"] == "running" && status["launch_id"] != launch {
            let _ = fs::remove_file(&marker);
            return Err("Экземпляр Леса сменился. Повторите команду для нового окна.".into());
        }
        if fs::metadata(&close_path).and_then(|m| m.modified()).ok() != before
            && read(&close_path).is_some_and(|v| v["phase"] == "blocked")
        {
            return Err("Лес не подтвердил сохранение диалога. Проверьте сообщение в окне.".into());
        }
        thread::sleep(Duration::from_millis(200));
    }
    let _ = fs::remove_file(&marker);
    Err("Лес не подтвердил остановку. Проверьте окно, затем les status и les logs.".into())
}

pub fn logs() -> Result<Value> {
    let directory = state_root()?.join("logs");
    let mut logs = Vec::new();
    for name in ["desktop-lifecycle.jsonl", "native-runtime-lifecycle.jsonl"] {
        let path = directory.join(name);
        let Ok(mut file) = File::open(&path) else {
            continue;
        };
        let Ok(length) = file.metadata().map(|meta| meta.len()) else {
            continue;
        };
        let start = length.saturating_sub(131_072);
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(start)).is_err()
            || file.take(131_072).read_to_end(&mut bytes).is_err()
        {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let events: Vec<Value> = text
            .lines()
            .rev()
            .filter_map(|line| serde_json::from_str(line).ok())
            .take(30)
            .collect();
        logs.push(json!({"file":name,"events":events.into_iter().rev().collect::<Vec<_>>() }));
    }
    Ok(json!({"directory":directory,"logs":logs}))
}
