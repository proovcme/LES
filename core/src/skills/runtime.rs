//! Select a packaged skill interpreter without importing application code.
#[derive(Debug)]
pub struct RuntimeFailure {
    pub code: &'static str,
    pub message: &'static str,
}
impl RuntimeFailure {
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}
type Failure = RuntimeFailure;
type Result<T> = std::result::Result<T, RuntimeFailure>;
impl From<RuntimeFailure> for crate::workspace::Failure {
    fn from(value: RuntimeFailure) -> Self {
        Self::new(value.code, value.message)
    }
}
use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
fn invalid() -> Failure {
    Failure::new(
        "SKILL_RUNTIME_INVALID",
        "Комплект внешних навыков повреждён. Восстановите установку Леса.",
    )
}
fn bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|_| invalid())?;
    if !file.metadata().map_err(|_| invalid())?.is_file() {
        return Err(invalid());
    }
    let mut value = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut value)
        .map_err(|_| invalid())?;
    if value.len() as u64 > limit {
        return Err(invalid());
    }
    Ok(value)
}
pub fn select(root: &Path, application: &Path, explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_absolute() || !path.is_file() {
            return Err(Failure::new(
                "SKILL_RUNTIME_MISSING",
                "Не найден явно выбранный исполнитель внешних навыков.",
            ));
        }
        return Ok(path);
    }
    let directory = root
        .parent()
        .ok_or_else(invalid)?
        .join("native/skill-python");
    if !directory.try_exists().map_err(|_| invalid())? {
        if root
            .parent()
            .unwrap()
            .join("native/skill-python-required.json")
            .try_exists()
            .map_err(|_| invalid())?
        {
            return Err(invalid());
        }
        return Ok(application.to_path_buf()); // Previous package compatibility only.
    }
    Ok(verified(&directory)?.0)
}
fn verified(directory: &Path) -> Result<(PathBuf, Value)> {
    let receipt: Value = serde_json::from_slice(&bytes(
        &directory.join("skill-python-receipt.json"),
        128 * 1024,
    )?)
    .map_err(|_| invalid())?;
    if receipt["schema"] != "les.skill-python.v1"
        || receipt["executable"] != "python.exe"
        || receipt["site_startup"] != false
        || receipt["application_import_path"] != false
    {
        return Err(invalid());
    }
    let program = directory.join("python.exe");
    if receipt["python_sha256"].as_str()
        != Some(crate::sha256_hex(&bytes(&program, 32 * 1024 * 1024)?).as_str())
    {
        return Err(invalid());
    }
    let stdlib = receipt["stdlib"]
        .as_str()
        .filter(|v| {
            v.starts_with("python")
                && v.ends_with(".zip")
                && v[6..v.len() - 4].bytes().all(|c| c.is_ascii_digit())
                && (2..=4).contains(&(v.len() - 10))
        })
        .ok_or_else(invalid)?;
    if receipt["stdlib_sha256"].as_str()
        != Some(crate::sha256_hex(&bytes(&directory.join(stdlib), 64 * 1024 * 1024)?).as_str())
    {
        return Err(invalid());
    }
    let startup = String::from_utf8(bytes(
        &directory.join(format!("{}._pth", &stdlib[..stdlib.len() - 4])),
        4096,
    )?)
    .map_err(|_| invalid())?;
    if startup.replace("\r\n", "\n") != format!("{stdlib}\n.\nLib/site-packages\n") {
        return Err(invalid());
    }
    if receipt["inventory"] != "skill-python-files.json"
        || receipt["inventory_sha256"].as_str()
            != Some(
                crate::sha256_hex(&bytes(
                    &directory.join("skill-python-files.json"),
                    32 * 1024 * 1024,
                )?)
                .as_str(),
            )
    {
        return Err(invalid());
    }
    Ok((program, receipt))
}

#[cfg(windows)]
pub fn verify_downloaded(directory: &Path, receipt_sha256: &str) -> Result<PathBuf> {
    if crate::sha256_hex(&bytes(
        &directory.join("skill-python-receipt.json"),
        128 * 1024,
    )?) != receipt_sha256
    {
        return Err(invalid());
    }
    Ok(verified(directory)?.0)
}

#[cfg(windows)]
pub fn unpack_download(
    mut file: File,
    destination: &Path,
    expected_size: u64,
    archive_sha256: &str,
    receipt_sha256: &str,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        io::{self, Seek},
    };

    if file.metadata().map_err(|_| invalid())?.len() != expected_size {
        return Err(invalid());
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 128 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| invalid())?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    if format!("{:x}", hash.finalize()) != archive_sha256 {
        return Err(invalid());
    }
    file.rewind().map_err(|_| invalid())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| invalid())?;
    const MAX_BYTES: u64 = 512 * 1024 * 1024;
    if archive.len() > 25_000 {
        return Err(invalid());
    }
    let mut extracted = 0;
    for index in 0..archive.len() {
        let mut member = archive.by_index(index).map_err(|_| invalid())?;
        let name = member.enclosed_name().ok_or_else(invalid)?;
        if !member.is_file()
            || name.components().any(|component| match component {
                std::path::Component::Normal(part) => part
                    .to_str()
                    .is_none_or(|part| part.contains(':') || part.ends_with(['.', ' '])),
                _ => true,
            })
            || member.size() > MAX_BYTES - extracted
        {
            return Err(invalid());
        }
        let path = destination.join(name);
        fs::create_dir_all(path.parent().ok_or_else(invalid)?).map_err(|_| invalid())?;
        let mut output = File::create_new(path).map_err(|_| invalid())?;
        let size = member.size();
        let written =
            io::copy(&mut member.by_ref().take(size + 1), &mut output).map_err(|_| invalid())?;
        if written != size {
            return Err(invalid());
        }
        extracted += written;
    }
    verify_downloaded(destination, receipt_sha256)?;
    Ok(())
}

/// Only builder-declared, existing directories inside the dependency tree.
pub(super) fn dependency_paths(program: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let directory = program.parent().ok_or_else(invalid)?;
    if !directory
        .join("skill-python-receipt.json")
        .try_exists()
        .map_err(|_| invalid())?
    {
        return Ok((Vec::new(), Vec::new()));
    }
    let (actual, receipt) = verified(directory)?;
    if actual.canonicalize().map_err(|_| invalid())?
        != program.canonicalize().map_err(|_| invalid())?
    {
        return Err(invalid());
    }
    let modules = receipt
        .get("module_roots")
        .cloned()
        .unwrap_or_else(|| serde_json::json!(["Lib/site-packages"]));
    let dlls = receipt
        .get("dll_roots")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let read = |values: &Value, maximum: usize| -> Result<Vec<PathBuf>> {
        let values = values
            .as_array()
            .filter(|v| v.len() <= maximum)
            .ok_or_else(invalid)?;
        let library = directory
            .join("Lib/site-packages")
            .canonicalize()
            .map_err(|_| invalid())?;
        values
            .iter()
            .map(|value| {
                let relative = Path::new(value.as_str().ok_or_else(invalid)?);
                if relative
                    .components()
                    .any(|v| !matches!(v, std::path::Component::Normal(_)))
                    || !relative.starts_with("Lib/site-packages")
                {
                    return Err(invalid());
                }
                let path = directory
                    .join(relative)
                    .canonicalize()
                    .map_err(|_| invalid())?;
                if !path.is_dir() || !path.starts_with(&library) {
                    return Err(invalid());
                }
                Ok(path)
            })
            .collect()
    };
    Ok((read(&modules, 64)?, read(&dlls, 16)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packaged_runtime_is_distinct_and_corruption_never_falls_back() {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).unwrap();
        let temp = std::env::temp_dir().join(format!(
            "les-skill-runtime-{:x}",
            u64::from_le_bytes(random)
        ));
        std::fs::create_dir(&temp).unwrap();
        let root = temp.join("runtime");
        std::fs::create_dir(&root).unwrap();
        let fallback = temp.join("application-python.exe");
        assert_eq!(select(&root, &fallback, None).unwrap(), fallback);
        std::fs::create_dir(temp.join("native")).unwrap();
        let required = temp.join("native/skill-python-required.json");
        std::fs::write(&required, "{\"required\":true}").unwrap();
        assert_eq!(
            select(&root, &fallback, None).unwrap_err().code,
            "SKILL_RUNTIME_INVALID"
        );
        let package = temp.join("native/skill-python");
        std::fs::create_dir_all(&package).unwrap();
        assert!(select(&root, &fallback, None).is_err());
        for (name, value) in [
            ("python.exe", b"executable".as_slice()),
            ("python313.zip", b"stdlib"),
            ("skill-python-files.json", b"{}"),
            ("python313._pth", b"python313.zip\n.\nLib/site-packages\n"),
        ] {
            std::fs::write(package.join(name), value).unwrap();
        }
        let receipt = serde_json::json!({"schema":"les.skill-python.v1","executable":"python.exe","python_sha256":crate::sha256_hex(b"executable"),
            "stdlib":"python313.zip","stdlib_sha256":crate::sha256_hex(b"stdlib"),"inventory":"skill-python-files.json",
            "inventory_sha256":crate::sha256_hex(b"{}"),"site_startup":false,"application_import_path":false});
        std::fs::write(
            package.join("skill-python-receipt.json"),
            receipt.to_string(),
        )
        .unwrap();
        assert_eq!(
            select(&root, &fallback, None).unwrap(),
            package.join("python.exe")
        );
        std::fs::write(
            package.join("python313._pth"),
            "python313.zip\n.\n../runtime\nimport site\n",
        )
        .unwrap();
        assert_eq!(
            select(&root, &fallback, None).unwrap_err().code,
            "SKILL_RUNTIME_INVALID"
        );
        std::fs::remove_dir_all(&temp).unwrap();
    }
}
