use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub root: PathBuf,
    pub state: PathBuf,
    pub python: PathBuf,
    pub qdrant: PathBuf,
    pub frontend: String,
    pub read_only: bool,
    pub parent_pipe: bool,
    pub browser: bool,
    pub native_api: bool,
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
                "--read-only" | "--parent-pipe" | "--browser" | "--native-api"
            ) {
                if !flags.insert(key) {
                    return Err(Failure::new(
                        "ARGUMENT_DUPLICATE",
                        "Параметр запуска повторяется.",
                    ));
                }
            } else if matches!(
                key.as_str(),
                "--root" | "--state" | "--python" | "--qdrant" | "--frontend"
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
        let python = path("--python")?;
        let qdrant = path("--qdrant")?;
        let frontend = values
            .remove("--frontend")
            .unwrap_or_else(|| "classic".into());
        if !matches!(frontend.as_str(), "classic" | "svelte") {
            return Err(Failure::new(
                "FRONTEND_INVALID",
                "Неизвестный интерфейс Леса.",
            ));
        }
        if !root.is_dir()
            || !python.is_file()
            || !qdrant.is_file()
            || !root.join("proxy/app.py").is_file()
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
        let native_api = flags.contains("--native-api");
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
        Ok(Self {
            root,
            state,
            python,
            qdrant,
            frontend,
            read_only: flags.contains("--read-only"),
            parent_pipe: flags.contains("--parent-pipe"),
            browser: flags.contains("--browser"),
            native_api,
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
        result.push((name.into(), value));
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
