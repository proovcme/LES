use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub root: PathBuf,
    pub state: PathBuf,
    pub skill_python: PathBuf,
    pub qdrant: PathBuf,
    pub read_only: bool,
    pub parent_pipe: bool,
    pub browser: bool,
    pub edge_storage: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: &'static str,
}

impl Failure {
    pub const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

pub type Result<T> = std::result::Result<T, Failure>;

impl Config {
    pub fn native_api(&self) -> PathBuf {
        self.root
            .with_file_name("native")
            .join(format!("api/les-api{}", std::env::consts::EXE_SUFFIX))
    }

    pub fn native_frontend(&self) -> PathBuf {
        self.root.join("desktop/light/frontend/dist")
    }

    pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = arguments.into_iter();
        let mut values = std::collections::HashMap::new();
        let mut flags = std::collections::HashSet::new();
        while let Some(key) = args.next() {
            if matches!(
                key.as_str(),
                "--read-only" | "--parent-pipe" | "--browser" | "--native-api" | "--native-only"
            ) {
                if !flags.insert(key) {
                    return Err(Failure::new(
                        "ARGUMENT_DUPLICATE",
                        "Параметр запуска повторяется.",
                    ));
                }
            } else if matches!(
                key.as_str(),
                "--root"
                    | "--state"
                    | "--python"
                    | "--skill-python"
                    | "--qdrant"
                    | "--frontend"
                    | "--edge-storage"
            ) {
                let value =
                    args.next()
                        .filter(|value| !value.starts_with("--"))
                        .ok_or(Failure::new(
                            "ARGUMENT_MISSING",
                            "Не указан обязательный параметр запуска.",
                        ))?;
                if values.insert(key, value).is_some() {
                    return Err(Failure::new(
                        "ARGUMENT_DUPLICATE",
                        "Параметр запуска повторяется.",
                    ));
                }
            } else {
                return Err(Failure::new(
                    "ARGUMENT_UNKNOWN",
                    "Неизвестный параметр запуска.",
                ));
            }
        }
        let mut path = |key: &str| -> Result<PathBuf> {
            let value = values.remove(key).map(PathBuf::from).ok_or(Failure::new(
                "ARGUMENT_MISSING",
                "Не указан обязательный путь запуска.",
            ))?;
            if !value.is_absolute() {
                return Err(Failure::new(
                    "PATH_NOT_ABSOLUTE",
                    "Для запуска нужен полный путь.",
                ));
            }
            Ok(value)
        };
        let root = path("--root")?;
        let state = path("--state")?;
        let qdrant = path("--qdrant")?;
        if values.contains_key("--python") {
            return Err(Failure::new(
                "NATIVE_ONLY_PYTHON_ARGUMENT",
                "Для внешних навыков укажите --skill-python.",
            ));
        }
        let edge_storage = values.remove("--edge-storage").map(PathBuf::from);
        if edge_storage
            .as_ref()
            .is_some_and(|p| !p.is_absolute() || p.is_file())
        {
            return Err(Failure::new(
                "EDGE_STORAGE_INVALID",
                "Нужен полный путь к отдельной папке индекса Edge.",
            ));
        }
        if values
            .get("--frontend")
            .is_some_and(|value| value != "svelte")
        {
            return Err(Failure::new(
                "NATIVE_ONLY_FRONTEND_REQUIRED",
                "Нативному запуску нужен интерфейс Svelte.",
            ));
        }
        if !root.is_dir() || (edge_storage.is_none() && !qdrant.is_file()) {
            return Err(Failure::new(
                "RUNTIME_MISSING",
                "В установке не хватает компонентов. Восстановите Лес; документы сохранятся.",
            ));
        }
        let skill_python = crate::skill_runtime::select(
            &root,
            std::path::Path::new(""),
            values.remove("--skill-python").map(PathBuf::from),
        )?;
        let config = Self {
            root,
            state,
            skill_python,
            qdrant,
            read_only: flags.contains("--read-only"),
            parent_pipe: flags.contains("--parent-pipe"),
            browser: flags.contains("--browser"),
            edge_storage,
        };
        if !config.native_frontend().join("index.html").is_file() {
            return Err(Failure::new(
                "FRONTEND_MISSING",
                "В установке отсутствует интерфейс. Восстановите приложение.",
            ));
        }
        if !config.native_api().is_file() {
            return Err(Failure::new(
                "NATIVE_API_MISSING",
                "В установке отсутствует нативный API. Восстановите приложение.",
            ));
        }
        Ok(config)
    }
}

pub fn child_environment(
    config: &Config,
    qdrant_url: &str,
    key: &str,
    api: u16,
    instance: &str,
) -> Vec<(String, String)> {
    const SYSTEM: &[&str] = &[
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "PATH",
        "PATHEXT",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "PROGRAMDATA",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "COMMONPROGRAMFILES",
        "HOME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "SHELL",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
        "HOMEDRIVE",
        "HOMEPATH",
    ];
    let mut result: Vec<_> = env::vars()
        .filter(|(key, _)| SYSTEM.contains(&key.to_uppercase().as_str()))
        .collect();
    let root = config.root.to_string_lossy().into_owned();
    let state = config.state.to_string_lossy().into_owned();
    let api = format!("http://127.0.0.1:{api}");
    for (name, value) in [
        ("LES_PRODUCT_EDITION", "light".into()),
        ("LES_STATE_ROOT", state.clone()),
        ("LES_WINDOWS_STATE_ROOT", state),
        (
            "LES_ENV_PATH",
            config.state.join(".env").to_string_lossy().into_owned(),
        ),
        ("LES_RUNTIME_HOME", root.clone()),
        ("LES_REPO_ROOT", root),
        (
            "RAG_META_DB_PATH",
            config
                .state
                .join("data/meta.db")
                .to_string_lossy()
                .into_owned(),
        ),
        ("QDRANT_URL", qdrant_url.into()),
        ("LES_LIGHT_QDRANT_URL", qdrant_url.into()),
        ("LES_LIGHT_QDRANT_API_KEY", key.into()),
        ("LES_LIGHT_INSTANCE_ID", instance.into()),
        ("PROXY_URL", api.clone()),
        ("LES_PROXY_URL", api.clone()),
        ("LES_STARTUP_MODEL_WARMUP", "false".into()),
        (
            "LES_STARTUP_BACKGROUND_MUTATIONS",
            (!config.read_only).to_string(),
        ),
        ("LES_FORMAT_WORKER_MODE", "native".into()),
    ] {
        result.push((name.into(), value));
    }
    if let Some(storage) = &config.edge_storage {
        result.push(("LES_NATIVE_EDGE".into(), "1".into()));
        result.push((
            "LES_INDEX_CONTENT_ROOT".into(),
            storage.join("content").to_string_lossy().into_owned(),
        ));
    }
    if let Some(readers) = config
        .root
        .parent()
        .map(|parent| parent.join("native/engineering-readers"))
        .filter(|path| path.join("reader-receipt.json").is_file())
    {
        result.push((
            "LES_ENGINEERING_READERS".into(),
            readers.to_string_lossy().into_owned(),
        ));
    }
    let native_api = config.native_api().to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        let pdfium = config.root.with_file_name("native").join("pdf/pdfium.dll");
        if pdfium.is_file() {
            result.push((
                "LES_NATIVE_PDFIUM".into(),
                pdfium.to_string_lossy().into_owned(),
            ));
            result.push(("LES_NATIVE_PDF_WORKER".into(), native_api.clone()));
        }
    }
    result.push(("LES_NATIVE_MCP_WORKER".into(), native_api.clone()));
    if !config.skill_python.as_os_str().is_empty() {
        result.push((
            "LES_SKILL_PYTHON".into(),
            config.skill_python.to_string_lossy().into_owned(),
        ));
    }
    let embedding = config.state.join("models/embeddings/active.json");
    if embedding.is_file() {
        result.push((
            "LES_NATIVE_EMBEDDING_SPEC".into(),
            embedding.to_string_lossy().into_owned(),
        ));
        result.push(("LES_NATIVE_EMBEDDING_WORKER".into(), native_api));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_and_duplicate_arguments_never_open_state() {
        assert_eq!(
            Config::parse(["--unknown".into()]).unwrap_err().code,
            "ARGUMENT_UNKNOWN"
        );
        assert_eq!(
            Config::parse(["--read-only".into(), "--read-only".into()])
                .unwrap_err()
                .code,
            "ARGUMENT_DUPLICATE"
        );
        assert_eq!(
            Config::parse(["--root".into(), "relative".into()])
                .unwrap_err()
                .code,
            "PATH_NOT_ABSOLUTE"
        );
    }
}
