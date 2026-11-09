//! The packaged updater verifies the installer; the supervisor runs it after shutdown.
use crate::{lifecycle, request_exit, validate_editor, Runtime};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::Manager;

const PLAN: &str = "desktop-update.json";
struct Operation<'a>(&'a AtomicBool);
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn script(directory: &Path, action: &str) -> Result<Command, String> {
    let windows = std::env::var_os("SystemRoot").ok_or("Не найдена папка Windows")?;
    let program = Path::new(&windows).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let helper = directory.join("runtime/tools/install-les.ps1");
    if !helper.is_file() {
        return Err("Компонент обновления отсутствует в установке.".into());
    }
    let mut command = Command::new(program);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(helper)
        .args(["-DesktopAction", action]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(command)
}

async fn invoke(mut command: Command) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let output = command
            .output()
            .map_err(|_| "Не удалось запустить проверку обновления.")?;
        if !output.status.success() {
            return Err(
                "Не удалось получить проверенный установщик. Проверьте интернет и свободное место."
                    .into(),
            );
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| "Не удалось прочитать результат обновления.".into())
    })
    .await
    .map_err(|_| "Не удалось завершить проверку обновления.")?
}

fn checksum(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn cache(state: &Path) -> PathBuf {
    state.join("artifacts/light-updates")
}

fn installer(state: &Path, value: &Value) -> Option<PathBuf> {
    let path = PathBuf::from(value["installer"].as_str()?);
    let root = cache(state).canonicalize().ok()?;
    let resolved = path.canonicalize().ok()?;
    let attempt = resolved.parent()?;
    let token = attempt.file_name()?.to_str()?.strip_prefix("download-")?;
    if token.len() != 32
        || !token.bytes().all(|c| c.is_ascii_hexdigit())
        || attempt.parent()? != root
        || resolved.file_name()?.to_str()? != "LES-RAG-Setup.exe"
        || !resolved.is_file()
        || !checksum(value["sha256"].as_str()?)
    {
        return None;
    }
    Some(path)
}

#[tauri::command]
pub async fn update_check(
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Runtime>,
) -> Result<Value, String> {
    validate_editor(&window, &runtime)?;
    invoke(script(&runtime.directory, "Check")?).await
}

#[tauri::command]
pub async fn update_install(
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Runtime>,
    build: u64,
    sha256: String,
) -> Result<Value, String> {
    validate_editor(&window, &runtime)?;
    if build == 0 || !checksum(&sha256) {
        return Err("Проверьте выпуск снова.".into());
    }
    let local = std::env::var_os("LOCALAPPDATA").ok_or("Не найдена папка установки.")?;
    let expected = Path::new(&local)
        .join("Programs/LES Light")
        .canonicalize()
        .ok();
    let expected_state = Path::new(&local).join("LES Light").canonicalize().ok();
    if expected.is_none()
        || runtime.directory.canonicalize().ok() != expected
        || expected_state.is_none()
        || runtime.state.canonicalize().ok() != expected_state
    {
        return Err("Установка из окна доступна в установленном Лесе.".into());
    }
    let launch = runtime
        .launch_id
        .as_ref()
        .ok_or("Перезапустите Лес перед обновлением.")?;
    if runtime.update_operation.swap(true, Ordering::AcqRel) {
        return Err("Обновление уже загружается.".into());
    }
    let _operation = Operation(&runtime.update_operation);
    if runtime
        .pending_update
        .lock()
        .map_err(|_| "Обновление занято.")?
        .is_some()
    {
        return Err("Обновление уже готовится к запуску.".into());
    }
    let mut command = script(&runtime.directory, "Download")?;
    command
        .arg("-OutputDirectory")
        .arg(cache(&runtime.state))
        .arg("-ExpectedBuild")
        .arg(build.to_string())
        .arg("-ExpectedSha")
        .arg(&sha256);
    let mut value = invoke(command).await?;
    if value["changed"] == true {
        return Err("Выпуск изменился. Проверьте обновления снова.".into());
    }
    if let Err(error) = validate_editor(&window, &runtime) {
        discard(&runtime.state, &value);
        return Err(error);
    }
    if value["sha256"] != sha256 || installer(&runtime.state, &value).is_none() {
        return Err("Установщик не прошёл проверку.".into());
    }
    if runtime
        .close
        .lock()
        .map_err(|_| "Закрытие занято.")?
        .request()
        .is_none()
    {
        discard(&runtime.state, &value);
        return Err("Окно ещё не готово сохранить диалог.".into());
    }
    value["launch_id"] = json!(launch);
    *runtime
        .pending_update
        .lock()
        .map_err(|_| "Обновление занято.")? = Some(value);
    request_exit(window.app_handle());
    Ok(json!({"ready":true}))
}

pub fn discard(state: &Path, value: &Value) {
    if let Some(path) = installer(state, value) {
        if fs::remove_file(&path).is_ok() {
            if let Some(attempt) = path.parent() {
                let _ = fs::remove_dir(attempt);
            }
        }
    }
}

pub fn arm(state: &Path, value: &Value) -> Result<(), String> {
    let staged = state.join("desktop-update.tmp");
    fs::write(&staged, value.to_string())
        .and_then(|_| fs::rename(staged, state.join(PLAN)))
        .map_err(|_| "Не удалось подготовить запуск установщика.".into())
}

pub fn launch(state: &Path, directory: &Path, launch_id: &str) -> Result<(), String> {
    let path = state.join(PLAN);
    let Ok(file) = fs::File::open(&path) else {
        return Ok(());
    };
    let mut bytes = Vec::new();
    file.take(16385)
        .read_to_end(&mut bytes)
        .map_err(|_| "Не удалось прочитать задание обновления.")?;
    let _ = fs::remove_file(&path);
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Задание обновления повреждено.")?;
    if value["launch_id"] != launch_id {
        return Ok(());
    }
    let file = installer(state, &value).ok_or("Проверенный установщик недоступен.")?;
    let mut command = script(directory, "Launch")?;
    command
        .arg("-InstallerPath")
        .arg(file)
        .arg("-ExpectedSha")
        .arg(value["sha256"].as_str().unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
        .spawn()
        .map_err(|_| "Не удалось открыть установщик.")?;
    lifecycle::event(state, "supervisor", "update_handoff_started", json!({}));
    Ok(())
}
