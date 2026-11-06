use crate::{
    config::{self, Config, Failure, Result},
    control,
    journal::Journal,
    probe::Probe,
};
use les_core::{prepare_child, JobGuard};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Service {
    name: &'static str,
    child: Child,
    guard: Option<JobGuard>,
}
impl Service {
    fn spawn(name: &'static str, command: &mut Command, log: File) -> Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(|_| {
                Failure::new("LOG_UNAVAILABLE", "Нет доступа к журналу службы.")
            })?))
            .stderr(Stdio::from(log));
        prepare_child(command);
        let mut child = command.spawn().map_err(|_| {
            Failure::new(
                "SERVICE_START_FAILED",
                "Система не смогла запустить службу Леса. Откройте журнал запуска.",
            )
        })?;
        let guard = match JobGuard::own(&child, None) {
            Ok(guard) => guard,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::new(
                    "SERVICE_OWNERSHIP_FAILED",
                    "Система не позволила изолировать службу Леса.",
                ));
            }
        };
        Ok(Self {
            name,
            child,
            guard: Some(guard),
        })
    }
    fn stop(&mut self) {
        let _ = self.child.kill(); // Exactly this owned root, never a name/PID scan.
        drop(self.guard.take()); // Descendants are stopped even if the root already exited.
        let _ = self.child.wait();
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct Stack {
    config: Config,
    services: Vec<Service>,
    qdrant_lock: Option<File>,
    config_path: PathBuf,
    pub api: u16,
    pub ui: u16,
    pub instance: String,
    qdrant_url: String,
    key: String,
    probe: Probe,
}

fn free_port() -> Result<u16> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|socket| socket.local_addr())
        .map(|address| address.port())
        .map_err(|_| {
            Failure::new(
                "PORT_UNAVAILABLE",
                "Не удалось выделить локальный порт. Повторите запуск.",
            )
        })
}
fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

impl Stack {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            config_path: config.state.join("qdrant/runtime.yaml"),
            config,
            services: Vec::new(),
            qdrant_lock: None,
            api: 0,
            ui: 0,
            instance: String::new(),
            qdrant_url: String::new(),
            key: String::new(),
            probe: Probe::new()?,
        })
    }
    fn log(&self, path: &str) -> Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.config.state.join(path))
            .map_err(|_| Failure::new("LOG_UNAVAILABLE", "Не удалось открыть журнал службы Леса."))
    }
    fn qdrant_start(&mut self, journal: &Journal) -> Result<()> {
        let directory = self.config.state.join("qdrant");
        fs::create_dir_all(&directory)
            .map_err(|_| Failure::new("STORAGE_UNAVAILABLE", "Нет доступа к хранилищу Леса."))?;
        if self.qdrant_lock.is_none() {
            self.qdrant_lock = Some(control::owner(&directory.join("runtime.lock"))?.ok_or(
                Failure::new(
                    "STORAGE_BUSY",
                    "Хранилище Леса уже используется другим процессом.",
                ),
            )?);
        }
        let root = fs::canonicalize(&directory).map_err(|_| {
            Failure::new(
                "STORAGE_UNAVAILABLE",
                "Не удалось проверить папку хранилища.",
            )
        })?;
        for attempt in 0..3 {
            let port = free_port()?;
            self.qdrant_url = format!("http://127.0.0.1:{port}");
            self.key = control::random_id()?;
            // JSON is YAML-compatible; paths and secrets cannot interpolate YAML.
            let configuration = json!({"storage":{"storage_path":root.join("storage"),"snapshots_path":root.join("snapshots")},
                "service":{"host":"127.0.0.1","http_port":port,"grpc_port":null,"api_key":self.key},
                "cluster":{"enabled":false},"telemetry_disabled":true});
            fs::write(&self.config_path, configuration.to_string()).map_err(|_| {
                Failure::new(
                    "STORAGE_UNAVAILABLE",
                    "Не удалось подготовить хранилище Леса.",
                )
            })?;
            let mut command = Command::new(&self.config.qdrant);
            command
                .args(["--config-path"])
                .arg(&self.config_path)
                .current_dir(&directory);
            for (name, _) in
                std::env::vars().filter(|(name, _)| name.to_uppercase().starts_with("QDRANT__"))
            {
                command.env_remove(name);
            }
            self.services.push(Service::spawn(
                "qdrant",
                &mut command,
                self.log("qdrant/runtime.log")?,
            )?);
            journal.event(
                "service_started",
                json!({"service":"qdrant","child_pid":self.services.last().unwrap().child.id()}),
            )?;
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut collision = false;
            while Instant::now() < deadline && !control::stopped() {
                if self
                    .services
                    .last_mut()
                    .unwrap()
                    .child
                    .try_wait()
                    .map_err(|_| {
                        Failure::new(
                            "SERVICE_STATUS_FAILED",
                            "Не удалось проверить службу хранилища.",
                        )
                    })?
                    .is_some()
                {
                    collision = !port_free(port);
                    break;
                }
                if self.probe.qdrant(&self.qdrant_url, &self.key, true) {
                    return Ok(());
                }
                control::pause(Duration::from_millis(100));
            }
            self.stop(journal);
            if control::stopped() {
                return Err(Failure::new("START_CANCELLED", "Запуск отменён."));
            }
            if !collision || attempt == 2 {
                return Err(Failure::new(
                    "STORAGE_START_FAILED",
                    "Хранилище Леса не подтвердило запуск. Откройте журнал хранилища.",
                ));
            }
            journal.event(
                "port_collision",
                json!({"service":"qdrant","attempt":attempt+1}),
            )?;
        }
        unreachable!()
    }
    pub fn start(&mut self, journal: &Journal) -> Result<String> {
        self.probe = Probe::new()?;
        for attempt in 0..3 {
            self.qdrant_start(journal)?;
            self.instance = control::random_id()?;
            if self.api == 0 || !port_free(self.api) {
                self.api = free_port()?;
            }
            if self.ui == 0 || !port_free(self.ui) {
                self.ui = free_port()?;
            }
            while self.ui == self.api {
                self.ui = free_port()?;
            }
            let environment = config::child_environment(
                &self.config,
                &self.qdrant_url,
                &self.key,
                self.api,
                self.ui,
                &self.instance,
            );
            let mut backend_port = self.api;
            if self.config.native_api {
                loop {
                    backend_port = free_port()?;
                    if backend_port != self.api && backend_port != self.ui {
                        break;
                    }
                }
            }
            let mut api = Command::new(&self.config.python);
            api.args([
                "-B",
                "-m",
                "uvicorn",
                "proxy.app:app",
                "--host",
                "127.0.0.1",
                "--port",
                &backend_port.to_string(),
            ]);
            let mut ui = Command::new(&self.config.python);
            ui.arg("-B").arg(self.config.root.join("sovushka_ng.py"));
            for command in [&mut api, &mut ui] {
                command
                    .current_dir(&self.config.state)
                    .env_clear()
                    .envs(environment.clone());
            }
            let private_tokens = if self.config.native_api {
                let model_token = control::random_id()?;
                let ingest_token = control::random_id()?;
                api.env("LES_INTERNAL_MODEL_TOKEN", &model_token).env(
                    "LES_INTERNAL_MODEL_URL",
                    format!(
                        "http://127.0.0.1:{}/api/light/internal-model-http",
                        self.api
                    ),
                ).env("LES_INTERNAL_INGEST_TOKEN", &ingest_token).env("LES_NATIVE_INDEX_URL",
                    format!("http://127.0.0.1:{}/api/light/internal-index-store", self.api));
                #[cfg(windows)]
                if let Some(python_root) = self.config.python.parent() {
                    let pdfium = python_root.join("Lib/site-packages/pypdfium2_raw/pdfium.dll");
                    if pdfium.is_file() {
                        api.env("LES_NATIVE_PDFIUM", pdfium).env(
                            "LES_NATIVE_PDF_WORKER",
                            self.config.root.parent().unwrap().join("native/api/les-api.exe"),
                        );
                    }
                }
                Some((model_token, ingest_token))
            } else {
                None
            };
            let mut commands = vec![
                (
                    if self.config.native_api {
                        "legacy_api"
                    } else {
                        "api"
                    },
                    api,
                    "logs/api-console.txt",
                ),
                ("ui", ui, "logs/ui-console.txt"),
            ];
            if let Some((model_token, ingest_token)) = private_tokens {
                let mut native = Command::new(self.config.root.parent().unwrap().join(format!(
                    "native/api/les-api{}",
                    std::env::consts::EXE_SUFFIX
                )));
                native
                    .arg("--listen")
                    .arg(format!("127.0.0.1:{}", self.api))
                    .arg("--upstream")
                    .arg(format!("http://127.0.0.1:{backend_port}"))
                    .arg("--database")
                    .arg(self.config.state.join("data/meta.db"))
                    .arg("--state")
                    .arg(&self.config.state)
                    .arg("--legacy-ui-url")
                    .arg(format!("http://127.0.0.1:{}/classic", self.ui))
                    .current_dir(&self.config.state)
                    .env_clear()
                    .envs(environment)
                    .env("LES_INTERNAL_MODEL_TOKEN", model_token)
                    .env("LES_INTERNAL_INGEST_TOKEN", ingest_token);
                if let Some(frontend) = self.config.native_frontend() {
                    native.arg("--frontend-assets").arg(frontend);
                }
                commands.push(("api", native, "logs/api-native-console.txt"));
            }
            // All services start before readiness probes, preserving parallel initialization.
            for (name, mut command, log) in commands {
                self.services
                    .push(Service::spawn(name, &mut command, self.log(log)?)?);
                journal.event(
                    "service_started",
                    json!({"service":name,"child_pid":self.services.last().unwrap().child.id()}),
                )?;
            }
            let deadline = Instant::now() + Duration::from_secs(75);
            loop {
                if control::stopped() {
                    return Err(Failure::new("START_CANCELLED", "Запуск отменён."));
                }
                if self.exited()?.is_some() {
                    break;
                }
                if self.probe.instance(&self.api_probe(), &self.instance)
                    && self.probe.instance(&self.ui_probe(), &self.instance)
                {
                    return Ok(self.ui_url());
                }
                if Instant::now() >= deadline {
                    return Err(Failure::new(
                        "SERVICE_START_TIMEOUT",
                        "Службы Леса не успели запуститься. Откройте журнал запуска.",
                    ));
                }
                control::pause(Duration::from_millis(200));
            }
            let collision = self.services.iter_mut().any(|service| {
                service.child.try_wait().ok().flatten().is_some()
                    && match service.name {
                        "api" => !port_free(self.api),
                        "ui" => !port_free(self.ui),
                        "legacy_api" => !port_free(backend_port),
                        _ => false,
                    }
            });
            self.stop(journal);
            if !collision || attempt == 2 {
                return Err(Failure::new(
                    "SERVICE_START_FAILED",
                    "Служба Леса завершилась при запуске. Откройте журнал запуска.",
                ));
            }
            // The failed pair is retired completely before any replacement starts.
            self.api = 0;
            self.ui = 0;
            journal.event(
                "port_collision",
                json!({"service":"api_ui","attempt":attempt+1}),
            )?;
        }
        unreachable!()
    }
    fn api_probe(&self) -> String {
        format!("http://127.0.0.1:{}/api/light/instance", self.api)
    }
    fn ui_probe(&self) -> String {
        format!("http://127.0.0.1:{}/healthz", self.ui)
    }
    pub fn ui_url(&self) -> String {
        if self.config.frontend == "svelte" {
            format!("http://127.0.0.1:{}/app/", self.api)
        } else {
            format!("http://127.0.0.1:{}/classic", self.ui)
        }
    }
    pub fn ready_fields(&self) -> Value {
        let pid = |name| {
            self.services
                .iter()
                .find(|service| service.name == name)
                .map(|service| service.child.id())
        };
        json!({"instance_id":self.instance,"ui_url":self.ui_url(),"api_url":format!("http://127.0.0.1:{}",self.api),
            "qdrant_pid":pid("qdrant"),"api_pid":pid("api"),"ui_pid":pid("ui"),
            "legacy_api_pid":pid("legacy_api"),"api_native":self.config.native_api})
    }
    fn exited(&mut self) -> Result<Option<Value>> {
        let mut dead = Vec::new();
        for service in &mut self.services {
            if let Some(status) = service.child.try_wait().map_err(|_| {
                Failure::new(
                    "SERVICE_STATUS_FAILED",
                    "Не удалось проверить состояние службы.",
                )
            })? {
                dead.push(json!({"service":service.name,"child_pid":service.child.id(),"exit_code":status.code()}));
            }
        }
        Ok((!dead.is_empty()).then(|| json!({"reason":"service_exit","services":dead})))
    }
    pub fn failed(&mut self) -> Result<Option<Value>> {
        if let Some(failure) = self.exited()? {
            return Ok(Some(failure));
        }
        let api = self.api_probe();
        let ui = self.ui_probe();
        Ok(self
            .probe
            .failed(&api, &ui, &self.instance, &self.qdrant_url, &self.key))
    }
    pub fn stop(&mut self, journal: &Journal) {
        for mut service in self.services.drain(..).rev() {
            service.stop();
            let _=journal.event("service_stopped",json!({"service":service.name,"child_pid":service.child.id(),
                "exit_code":service.child.try_wait().ok().flatten().and_then(|status|status.code())}));
        }
        let _ = fs::remove_file(&self.config_path);
        self.key.clear();
        self.qdrant_url.clear();
    }
}
