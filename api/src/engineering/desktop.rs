use super::Result;
use std::{
    ffi::OsString,
    os::windows::{ffi::OsStringExt, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, sync::Mutex, time};
use windows_sys::Win32::{
    Foundation::ERROR_SUCCESS,
    System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ},
};

const START_LIMIT: Duration = Duration::from_secs(90);
static LAST_FAILURE: Mutex<Option<(Instant, String)>> = Mutex::const_new(None);

pub(super) fn matches(url: &reqwest::Url) -> bool {
    let port = std::env::var("UVYAZKA_DESKTOP_PORT")
        .ok()
        .map_or(Some(17881), |value| value.parse().ok());
    url.host_str() == Some("127.0.0.1") && port.is_some() && url.port() == port
}

pub(super) async fn ensure_started(port: u16) -> Result<()> {
    let mut last_failure = LAST_FAILURE.lock().await;
    let address = (std::net::Ipv4Addr::LOCALHOST, port);
    if listening(address).await {
        return Ok(());
    }
    if let Some((when, code)) = &*last_failure {
        if when.elapsed() < Duration::from_secs(3) {
            return Err(code.clone());
        }
    }
    let result = start(address).await;
    *last_failure = result
        .as_ref()
        .err()
        .map(|code| (Instant::now(), code.clone()));
    result
}

async fn listening(address: (std::net::Ipv4Addr, u16)) -> bool {
    matches!(
        time::timeout(Duration::from_millis(300), TcpStream::connect(address)).await,
        Ok(Ok(_))
    )
}

async fn start(address: (std::net::Ipv4Addr, u16)) -> Result<()> {
    let executable = installed_executable().ok_or("ENGINEERING_DESKTOP_NOT_INSTALLED")?;
    let directory = executable
        .parent()
        .ok_or("ENGINEERING_DESKTOP_START_FAILED")?;
    // Do not pass LES API and worker credentials to the companion.
    Command::new(&executable)
        .current_dir(directory)
        .env_clear()
        .envs(std::env::vars_os().filter(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("LES_")
        }))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW for console subsystems
        .spawn()
        .map_err(|_| "ENGINEERING_DESKTOP_START_FAILED")?;
    time::timeout(START_LIMIT, async {
        while !listening(address).await {
            time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .map_err(|_| "ENGINEERING_DESKTOP_START_TIMEOUT".into())
}

fn installed_executable() -> Option<PathBuf> {
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Увязка\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "InstallLocation\0".encode_utf16().collect();
    let mut value = vec![0u16; 32768];
    let mut byte_count = (value.len() * size_of::<u16>()) as u32;
    // RegGetValueW writes at most byte_count bytes to our UTF-16 buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            value.as_mut_ptr().cast(),
            &mut byte_count,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = value.iter().position(|&c| c == 0)?;
    let value = OsString::from_wide(&value[..len]);
    let value = value.to_str()?.trim_matches('"');
    let directory = PathBuf::from(value);
    if !directory.is_absolute() {
        return None;
    }
    let executable = directory.join("uvyazka-desktop.exe");
    executable.is_file().then_some(executable)
}
