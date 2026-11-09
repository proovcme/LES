use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub root: PathBuf,
    pub state: PathBuf,
    pub python: Option<PathBuf>,
    pub skill_python: PathBuf,
    pub qdrant: PathBuf,
    pub frontend: String,
    pub read_only: bool,
    pub parent_pipe: bool,
    pub browser: bool,
    pub native_api: bool,
    pub native_only: bool,
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
    pub fn native_frontend(&self) -> Option<PathBuf> {
        let root = self.root.join("desktop/light/frontend/dist");
        (self.native_api
            && (self.frontend == "svelte" || root.join("build-receipt.json").is_file()))
        .then_some(root)
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
        let native_only = flags.contains("--native-only");
        let python = if native_only {
            None
        } else {
            Some(path("--python")?)
        };
        let qdrant = path("--qdrant")?;
        if native_only && values.contains_key("--python") {
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
        let frontend = values.remove("--frontend").unwrap_or_else(|| {
            if native_only {
                "svelte".into()
            } else {
                "classic".into()
            }
        });
        if !matches!(frontend.as_str(), "classic" | "svelte") {
            return Err(Failure::new(
                "FRONTEND_INVALID",
                "Неизвестный интерфейс Леса.",
            ));
        }
        if native_only && frontend != "svelte" {
            return Err(Failure::new(
                "NATIVE_ONLY_FRONTEND_REQUIRED",
                "Нативному запуску нужен интерфейс Svelte.",
            ));
        }
        if !root.is_dir()
            || python.as_ref().is_some_and(|p| !p.is_file())
            || (edge_storage.is_none() && !qdrant.is_file())
            || (!native_only && !root.join("proxy/app.py").is_file())
        {
            return Err(Failure::new(
                "RUNTIME_MISSING",
                "В установке не хватает компонентов. Восстановите Лес; документы сохранятся.",
            ));
        }
        if frontend == "svelte"
            && !root
                .join("desktop/light/frontend/dist/index.html")
                .is_file()
        {
            return Err(Failure::new(
                "FRONTEND_MISSING",
                "В установке отсутствует новый интерфейс. Восстановите приложение.",
            ));
        }
        let native_api = native_only || flags.contains("--native-api");
        if edge_storage.is_some() && !native_api {
            return Err(Failure::new(
                "EDGE_NATIVE_API_REQUIRED",
                "Edge требует нативный API Леса.",
            ));
        }
        if native_api
            && !root.parent().is_some_and(|parent| {
                parent
                    .join(format!(
                        "native/api/les-api{}",
                        std::env::consts::EXE_SUFFIX
                    ))
                    .is_file()
            })
        {
            return Err(Failure::new(
                "NATIVE_API_MISSING",
                "В установке отсутствует нативный API. Восстановите приложение.",
            ));
        }
        let skill_python = crate::skill_runtime::select(
            &root,
            python.as_deref().unwrap_or(std::path::Path::new("")),
            values.remove("--skill-python").map(PathBuf::from),
        )?;
        if native_only && !skill_python.is_file() {
            return Err(Failure::new(
                "SKILL_RUNTIME_MISSING",
                "Не найден отдельный исполнитель внешних навыков.",
            ));
        }
        Ok(Self {
            root,
            state,
            python,
            skill_python,
            qdrant,
            frontend,
            read_only: flags.contains("--read-only"),
            parent_pipe: flags.contains("--parent-pipe"),
            browser: flags.contains("--browser"),
            native_api,
            native_only,
            edge_storage,
        })
    }
}

pub fn child_environment(
    config: &Config,
    qdrant_url: &str,
    key: &str,
    api: u16,
    ui: u16,
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
        ("PYTHONPATH", root.clone()),
        ("PYTHONUTF8", "1".into()),
        ("PYTHONNOUSERSITE", "1".into()),
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
        (
            "LES_LIGHT_LEGACY_UI_URL",
            format!("http://127.0.0.1:{ui}/classic"),
        ),
        ("LES_LIGHT_SVELTE_URL", format!("{api}/app/")),
        (
            "LES_NATIVE_FRONTEND",
            config.native_frontend().is_some().to_string(),
        ),
        ("SOVUSHKA_UI_PORT", ui.to_string()),
        ("SOVUSHKA_UI_HOST", "127.0.0.1".into()),
        (
            "NICEGUI_STORAGE_PATH",
            config.state.join(".nicegui").to_string_lossy().into_owned(),
        ),
        ("LES_STARTUP_MODEL_WARMUP", "false".into()),
        (
            "LES_STARTUP_BACKGROUND_MUTATIONS",
            (!config.read_only).to_string(),
        ),
        ("LES_FORMAT_WORKER_MODE", "native".into()),
    ] {
        if config.native_only
            && matches!(
                name,
                "PYTHONPATH"
                    | "PYTHONUTF8"
                    | "PYTHONNOUSERSITE"
                    | "LES_LIGHT_LEGACY_UI_URL"
                    | "SOVUSHKA_UI_PORT"
                    | "SOVUSHKA_UI_HOST"
                    | "NICEGUI_STORAGE_PATH"
            )
        {
            continue;
        }
        result.push((name.into(), value));
    }
    if let Some(storage) = &config.edge_storage {
        result.push(("LES_NATIVE_EDGE".into(), "1".into()));
        result.push((
            "LES_INDEX_CONTENT_ROOT".into(),
            storage.join("content").to_string_lossy().into_owned(),
        ));
    }
    #[cfg(windows)]
    if config.native_api {
        let native = config.root.parent().unwrap().join("native/pdf/pdfium.dll");
        let compatible = config
            .python
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(|p| p.join("Lib/site-packages/pypdfium2_raw/pdfium.dll"));
        if let Some(asset) = std::iter::once(native)
            .chain(compatible)
            .find(|p| p.is_file())
        {
            result.push((
                "LES_NATIVE_PDFIUM".into(),
                asset.to_string_lossy().into_owned(),
            ));
            result.push((
                "LES_NATIVE_PDF_WORKER".into(),
                config
                    .root
                    .parent()
                    .unwrap()
                    .join("native/api/les-api.exe")
                    .to_string_lossy()
                    .into_owned(),
            ));
        }
    }
    result.push((
        "LES_NATIVE_MCP_WORKER".into(),
        config
            .root
            .parent()
            .unwrap()
            .join(format!(
                "native/api/les-api{}",
                std::env::consts::EXE_SUFFIX
            ))
            .to_string_lossy()
            .into_owned(),
    ));
    result.push((
        "LES_SKILL_PYTHON".into(),
        config.skill_python.to_string_lossy().into_owned(),
    ));
    let embedding = config.state.join("models/embeddings/active.json");
    if config.native_api && embedding.is_file() {
        result.push((
            "LES_NATIVE_EMBEDDING_SPEC".into(),
            embedding.to_string_lossy().into_owned(),
        ));
        result.push((
            "LES_NATIVE_EMBEDDING_WORKER".into(),
            config
                .root
                .parent()
                .unwrap()
                .join(format!(
                    "native/api/les-api{}",
                    std::env::consts::EXE_SUFFIX
                ))
                .to_string_lossy()
                .into_owned(),
        ));
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
