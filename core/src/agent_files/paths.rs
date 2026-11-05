use super::*;
use std::fs;

pub(super) fn regular(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    return Err(failure(
                        "AGENT_PATH_UNSAFE",
                        "В рабочем каталоге обнаружена ссылка. Выберите обычную папку.",
                    ));
                }
            }
            if meta.file_type().is_symlink() {
                return Err(failure(
                    "AGENT_PATH_UNSAFE",
                    "Ссылки в рабочем каталоге недоступны.",
                ));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(unavailable()),
    }
}
pub(super) fn base(root: &Path, create: bool) -> Result<Option<PathBuf>> {
    if !root.is_absolute() {
        return Err(failure("AGENT_INVALID", "Нужен каталог состояния Леса."));
    }
    if !root.try_exists().map_err(|_| unavailable())? {
        if !create {
            return Ok(None);
        }
        fs::create_dir_all(root).map_err(|_| unavailable())?;
    }
    let root = fs::canonicalize(root).map_err(|_| unavailable())?;
    let path = root.join("agent");
    if !regular(&path)? {
        if !create {
            return Ok(None);
        }
        if let Err(e) = fs::create_dir(&path) {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(unavailable());
            }
        }
        regular(&path)?;
    }
    if !path.is_dir() {
        return Err(unavailable());
    }
    Ok(Some(path))
}
pub(super) fn relative(args: &Map<String, Value>) -> Result<String> {
    let project = project(args)?;
    match text(args, "area")? {
        "user" => Ok("USER.md".into()),
        "memory" => Ok(if project == 0 {
            "MEMORY.md".into()
        } else {
            format!("projects/{project}/MEMORY.md")
        }),
        "policy" => Ok("policy.json".into()),
        "files" => {
            let name = text(args, "path")?;
            validate_relative(name)?;
            if name.is_empty() {
                return Err(failure("AGENT_INVALID", "Укажите имя рабочего файла."));
            }
            Ok(format!("workspaces/{project}/{name}"))
        }
        _ => Err(failure("AGENT_INVALID", "Неизвестный раздел памяти.")),
    }
}
pub(super) fn validate_relative(name: &str) -> Result<()> {
    if name.contains('\\')
        || name.contains(':')
        || name.contains('\0')
        || name.starts_with('/')
        || name.chars().count() > 240
    {
        return Err(unsafe_path());
    }
    for part in name.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || part.starts_with('.')
            || part.ends_with(['.', ' '])
            || part
                .chars()
                .any(|c| c.is_control() || "<>\"|?*".contains(c))
            || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.ends_with(['1', '2', '3', '4', '5', '6', '7', '8', '9']))
        {
            return Err(unsafe_path());
        }
    }
    Ok(())
}
pub(super) fn target(base: &Path, relative: &str, create: bool) -> Result<PathBuf> {
    validate_relative(relative)?;
    let mut current = base.to_path_buf();
    let parts = relative.split('/').collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        let exists = regular(&current)?;
        if index + 1 < parts.len() {
            if !exists && create {
                if let Err(e) = fs::create_dir(&current) {
                    if e.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(unavailable());
                    }
                }
                regular(&current)?;
            }
            if exists && !current.is_dir() {
                return Err(unsafe_path());
            }
        } else if exists && !current.is_file() {
            return Err(unsafe_path());
        }
    }
    Ok(current)
}
fn unsafe_path() -> Failure {
    failure(
        "AGENT_PATH_UNSAFE",
        "Имя файла недоступно. Используйте относительный путь внутри рабочего каталога Леса.",
    )
}
