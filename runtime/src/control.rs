use crate::config::{Failure, Result};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    thread,
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
static REASON: AtomicU8 = AtomicU8::new(0);
pub fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}
pub fn request_stop(reason: u8) {
    let _ = REASON.compare_exchange(0, reason, Ordering::SeqCst, Ordering::SeqCst);
    STOP.store(true, Ordering::SeqCst);
}
pub fn stop_reason() -> &'static str {
    match REASON.load(Ordering::SeqCst) {
        1 => "desktop_stop",
        2 => "parent_pipe_eof",
        3 => "invalid_parent_command",
        4 => "signal",
        _ => "stop_requested",
    }
}
pub fn pause(duration: Duration) {
    let deadline = Instant::now() + duration;
    while !stopped() && Instant::now() < deadline {
        thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50)),
        );
    }
}

#[cfg(windows)]
unsafe extern "system" fn signal_handler(event: u32) -> i32 {
    if matches!(event, 0 | 1 | 2 | 5 | 6) {
        request_stop(4);
        1
    } else {
        0
    }
}
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}
#[cfg(unix)]
unsafe extern "C" fn signal_handler(_: i32) {
    request_stop(4);
}
#[cfg(unix)]
unsafe extern "C" {
    fn signal(number: i32, handler: unsafe extern "C" fn(i32)) -> usize;
}

pub fn install(parent_pipe: bool) {
    #[cfg(windows)]
    unsafe {
        SetConsoleCtrlHandler(Some(signal_handler), 1);
    }
    #[cfg(unix)]
    unsafe {
        signal(2, signal_handler);
        signal(15, signal_handler);
    }
    if parent_pipe {
        thread::spawn(|| {
            // A bounded command or EOF: the desktop can never leave an orphan owner.
            let stdin = std::io::stdin();
            let mut input = stdin.lock().take(64);
            let mut bytes = Vec::new();
            let _ = input.read_until(b'\n', &mut bytes);
            let command = String::from_utf8_lossy(&bytes);
            request_stop(if bytes.is_empty() {
                2
            } else if command.trim() == "stop" {
                1
            } else {
                3
            });
        });
    }
}

pub fn owner(path: &Path) -> Result<Option<File>> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| Failure::new("OWNER_UNAVAILABLE", "Нет доступа к папке данных Леса."))?;
    // A non-empty byte also preserves compatibility with the previous msvcrt lock.
    if file
        .metadata()
        .map_err(|_| {
            Failure::new(
                "OWNER_UNAVAILABLE",
                "Не удалось проверить владельца запуска.",
            )
        })?
        .len()
        == 0
    {
        file.write_all(b"0").map_err(|_| {
            Failure::new(
                "OWNER_UNAVAILABLE",
                "Не удалось сохранить владельца запуска.",
            )
        })?;
    }
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(_) => Err(Failure::new(
            "OWNER_UNAVAILABLE",
            "Не удалось проверить владельца запуска.",
        )),
    }
}

pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| {
        Failure::new(
            "RANDOM_UNAVAILABLE",
            "Система не позволила защитить локальные службы. Повторите запуск.",
        )
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_file_owner_and_random_secrets() {
        let root = std::env::temp_dir().join(format!("les-native-owner-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let first = owner(&root.join("lock")).unwrap().unwrap();
        assert!(owner(&root.join("lock")).unwrap().is_none());
        drop(first);
        assert!(owner(&root.join("lock")).unwrap().is_some());
        assert_ne!(random_id().unwrap(), random_id().unwrap());
        std::fs::remove_dir_all(root).unwrap();
    }
}
