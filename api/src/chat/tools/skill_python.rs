//! Install the pinned Windows skill interpreter only for an approved calculation.
use les_core::skills::runtime::RuntimeFailure;
#[cfg(windows)]
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

#[cfg(windows)]
use std::fs::{self, File};

const MISSING: &str = "SKILL_RUNTIME_COMPONENT_MISSING";

pub(super) struct Pending(pub(super) std::sync::Arc<AtomicBool>);
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn unavailable() -> RuntimeFailure {
    RuntimeFailure::new(MISSING, "Не найден исполнитель внешних Python-навыков.")
}

pub(super) fn resolve(
    state: &Path,
    end: Instant,
    cancelled: &AtomicBool,
) -> Result<PathBuf, RuntimeFailure> {
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= end {
        return Err(unavailable());
    }
    if let Some(program) = std::env::var_os("LES_SKILL_PYTHON").filter(|v| !v.is_empty()) {
        let program = PathBuf::from(program);
        return if program.is_absolute() && program.is_file() {
            Ok(program)
        } else {
            Err(unavailable())
        };
    }
    #[cfg(windows)]
    {
        let descriptor = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()?
                    .parent()
                    .map(|dir| dir.join("skill-python-download.json"))
            })
            .ok_or_else(unavailable)?;
        let asset = Asset::read(&descriptor)?;
        let version: Value = serde_json::from_str(include_str!("../../../../config/version.json"))
            .map_err(|_| invalid())?;
        let url = format!(
            "https://github.com/proovcme/LES/releases/download/v{}/LES-Skill-Python-{}.zip",
            version["product_version"].as_str().ok_or_else(invalid)?,
            version["build_number"].as_u64().ok_or_else(invalid)?
        );
        return install(state, &asset, end, cancelled, |path| {
            fetch(&url, path, asset.bytes, end, cancelled)
        });
    }
    #[cfg(not(windows))]
    {
        let _ = (state, end, cancelled);
        Err(unavailable())
    }
}

#[cfg(windows)]
struct Asset {
    bytes: u64,
    sha256: String,
    receipt_sha256: String,
}

#[cfg(windows)]
impl Asset {
    fn read(path: &Path) -> Result<Self, RuntimeFailure> {
        use std::io::Read;
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    unavailable()
                } else {
                    invalid()
                }
            })?
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if bytes.len() > 4096 || value["schema"] != "les.skill-python-download.v1" {
            return Err(invalid());
        }
        let hash = |key: &str| {
            value[key]
                .as_str()
                .filter(|v| {
                    v.len() == 64
                        && v.bytes()
                            .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))
                })
                .map(str::to_owned)
                .ok_or_else(invalid)
        };
        Ok(Self {
            bytes: value["bytes"]
                .as_u64()
                .filter(|n| (1..=384 * 1024 * 1024).contains(n))
                .ok_or_else(invalid)?,
            sha256: hash("sha256")?,
            receipt_sha256: hash("receipt_sha256")?,
        })
    }
}

#[cfg(windows)]
fn invalid() -> RuntimeFailure {
    RuntimeFailure::new(
        "SKILL_RUNTIME_INVALID",
        "Не удалось проверить комплект Python для внешних навыков.",
    )
}

#[cfg(windows)]
fn download_failed() -> RuntimeFailure {
    RuntimeFailure::new(
        "SKILL_RUNTIME_DOWNLOAD_FAILED",
        "Не удалось скачать Python. Проверьте доступ к GitHub и повторите запуск навыка.",
    )
}

#[cfg(windows)]
fn storage_error() -> RuntimeFailure {
    RuntimeFailure::new(
        "SKILL_RUNTIME_STORAGE_FAILED",
        "Не удалось сохранить Python. Проверьте место на диске и доступ к папке данных Леса.",
    )
}

#[cfg(windows)]
fn install(
    state: &Path,
    asset: &Asset,
    end: Instant,
    cancelled: &AtomicBool,
    download: impl FnOnce(&Path) -> Result<(), RuntimeFailure>,
) -> Result<PathBuf, RuntimeFailure> {
    use les_core::skills::runtime::{unpack_download, verify_downloaded};
    use std::fs::TryLockError;
    let parent = state.join("components/skill-python");
    let installed = parent.join(&asset.sha256);
    if let Ok(program) = verify_downloaded(&installed, &asset.receipt_sha256) {
        active(end, cancelled)?;
        return Ok(program);
    }
    fs::create_dir_all(&parent).map_err(|_| storage_error())?;
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(format!("{}.lock", asset.sha256)))
        .map_err(|_| storage_error())?;
    loop {
        active(end, cancelled)?;
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            Err(_) => return Err(storage_error()),
        }
    }
    if let Ok(program) = verify_downloaded(&installed, &asset.receipt_sha256) {
        active(end, cancelled)?;
        return Ok(program);
    }
    struct Staging(PathBuf);
    impl Drop for Staging {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let stage = parent.join(format!(".{}.installing", asset.sha256));
    remove_component(&stage)?;
    fs::create_dir(&stage).map_err(|_| storage_error())?;
    let stage = Staging(stage);
    let archive = stage.0.join("component.zip");
    let runtime = stage.0.join("runtime");
    download(&archive)?;
    active(end, cancelled)?;
    unpack_download(
        File::open(archive).map_err(|_| download_failed())?,
        &runtime,
        asset.bytes,
        &asset.sha256,
        &asset.receipt_sha256,
    )?;
    active(end, cancelled)?;
    remove_component(&installed)?;
    fs::rename(runtime, &installed).map_err(|_| storage_error())?;
    active(end, cancelled)?;
    Ok(installed.join("python.exe"))
}

#[cfg(windows)]
fn remove_component(path: &Path) -> Result<(), RuntimeFailure> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(storage_error()),
    }
}

#[cfg(windows)]
fn active(end: Instant, cancelled: &AtomicBool) -> Result<(), RuntimeFailure> {
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= end {
        Err(download_failed())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn fetch(
    url: &str,
    path: &Path,
    expected_bytes: u64,
    end: Instant,
    cancelled: &AtomicBool,
) -> Result<(), RuntimeFailure> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::Duration,
    };
    let curl = crate::external_embed::executable().ok_or_else(download_failed)?;
    let mut url = reqwest::Url::parse(url).map_err(|_| invalid())?;
    for hop in 0..=3 {
        active(end, cancelled)?;
        let allowed_host = if hop == 0 {
            url.host_str() == Some("github.com")
        } else {
            matches!(
                url.host_str(),
                Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
            )
        };
        if url.scheme() != "https"
            || !allowed_host
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(download_failed());
        }
        let remaining = end.saturating_duration_since(Instant::now());
        let mut command = Command::new(&curl);
        command
            .args([
                "--disable",
                "--silent",
                "--globoff",
                "--proto",
                "=https",
                "--connect-timeout",
                "10",
            ])
            .arg("--max-time")
            .arg(format!("{:.3}", remaining.as_secs_f64().max(0.05)))
            .arg("--max-filesize")
            .arg(expected_bytes.to_string())
            .arg("--output")
            .arg(path)
            .arg("--write-out")
            .arg("%{http_code}\n%{redirect_url}")
            .arg("--url")
            .arg(url.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        les_core::prepare_child(&mut command);
        let mut child = command.spawn().map_err(|_| download_failed())?;
        let guard = les_core::JobGuard::own(&child, None);
        let status = loop {
            if guard.is_err()
                || active(end, cancelled).is_err()
                || fs::metadata(path).is_ok_and(|file| file.len() > expected_bytes)
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err(download_failed());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(download_failed());
                }
            }
        };
        if !status.success() {
            return Err(download_failed());
        }
        let mut reply = String::new();
        child
            .stdout
            .take()
            .ok_or_else(download_failed)?
            .take(8192)
            .read_to_string(&mut reply)
            .map_err(|_| download_failed())?;
        let (code, redirect) = reply.split_once('\n').ok_or_else(download_failed)?;
        match code.trim() {
            "200" => return Ok(()),
            "301" | "302" | "303" | "307" | "308" => {
                url = reqwest::Url::parse(redirect.trim()).map_err(|_| download_failed())?;
            }
            _ => return Err(download_failed()),
        }
    }
    Err(download_failed())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{cell::Cell, time::Duration};

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "les-skill-download-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn interrupted_or_invalid_download_never_creates_a_cached_interpreter() {
        let root = scratch();
        let asset = Asset {
            bytes: 4,
            sha256: "a".repeat(64),
            receipt_sha256: "b".repeat(64),
        };
        let cancelled = AtomicBool::new(false);
        let end = Instant::now() + Duration::from_secs(20);
        let rejected = install(&root, &asset, end, &cancelled, |path| {
            fs::write(path, b"bad!").unwrap();
            Ok(())
        });
        assert_eq!(rejected.unwrap_err().code, "SKILL_RUNTIME_INVALID");
        let parent = root.join("components/skill-python");
        assert!(!parent.join(&asset.sha256).exists());
        assert!(!parent
            .join(format!(".{}.installing", asset.sha256))
            .exists());

        let interrupted = install(&root, &asset, end, &cancelled, |path| {
            fs::write(path, b"bad").unwrap();
            cancelled.store(true, Ordering::Relaxed);
            Ok(())
        });
        assert_eq!(
            interrupted.unwrap_err().code,
            "SKILL_RUNTIME_DOWNLOAD_FAILED"
        );
        assert!(!parent.join(&asset.sha256).exists());
        assert!(!parent
            .join(format!(".{}.installing", asset.sha256))
            .exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires the release candidate archive and descriptor"]
    fn packaged_archive_provisions_once_and_runs_offline() {
        let source = PathBuf::from(std::env::var_os("LES_TEST_OPTIONAL_SKILL_ARCHIVE").unwrap());
        let descriptor =
            PathBuf::from(std::env::var_os("LES_TEST_OPTIONAL_SKILL_DESCRIPTOR").unwrap());
        let asset = Asset::read(&descriptor).unwrap();
        let root = scratch();
        let cancelled = AtomicBool::new(false);
        let end = Instant::now() + Duration::from_secs(120);
        let calls = Cell::new(0);
        let installed = install(&root, &asset, end, &cancelled, |path| {
            calls.set(calls.get() + 1);
            fs::copy(&source, path).unwrap();
            Ok(())
        })
        .unwrap();
        let reusable = install(&root, &asset, end, &cancelled, |_| {
            panic!("cached runtime must be used")
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(installed, reusable);

        let library = root.join("skills");
        fs::create_dir_all(library.join("example")).unwrap();
        fs::write(library.join("example/SKILL.md"), "Test calculation").unwrap();
        let source = b"import numpy as np\ndef calculate(value):\n    return {'value': int(np.asarray([value['n']])[0]) * 2}\n";
        fs::write(library.join("example/calculate.py"), source).unwrap();
        fs::write(
            library.join("approved-compute.json"),
            serde_json::to_vec(&serde_json::json!([{
                "skill":"example","path":"calculate.py","entrypoint":"calculate",
                "sha256":les_core::sha256_hex(source)
            }]))
            .unwrap(),
        )
        .unwrap();
        let response = les_core::skills::execute_with_runtime(
            &serde_json::json!({
                "schema":"les.skills.v1","library":library,"artifacts":root.join("artifacts"),
                "action":"calculate","skill":"example","path":"calculate.py",
                "items":[{"id":"first","input":{"n":21}}]
            }),
            || Ok(reusable),
        );
        assert_eq!(
            response["value"]["rows"][0]["result"]["value"], 42,
            "{response}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
