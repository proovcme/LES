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
        if !self.config.read_only {
            match les_core::index_cache::remove_abandoned(&self.config.state) {
                Ok(0) => (),
                Ok(count) => journal.event("index_spools_removed", json!({"count":count}))?,
                Err(_) => journal.event("index_spool_cleanup_unavailable", json!({}))?,
            }
        }
        for attempt in 0..3 {
            if self.config.edge_storage.is_none() {
                self.qdrant_start(journal)?;
            }
            self.instance = control::random_id()?;
            if self.api == 0 || !port_free(self.api) {
                self.api = free_port()?;
            }
            if self.config.edge_storage.is_some() {
                self.qdrant_url = format!("http://127.0.0.1:{}", self.api);
                self.key = control::random_id()?;
                journal.event(
                    "edge_opening",
                    json!({"engine":"qdrant-edge","separate_database_process":false}),
                )?;
            }
            let environment = config::child_environment(
                &self.config,
                &self.qdrant_url,
                &self.key,
                self.api,
                &self.instance,
            );
            let mut api = Command::new(self.config.native_api());
            api.arg("--listen")
                .arg(format!("127.0.0.1:{}", self.api))
                .arg("--database")
                .arg(self.config.state.join("data/meta.db"))
                .arg("--state")
                .arg(&self.config.state)
                .arg("--chat-owner")
                .arg("rust")
                .arg("--frontend-assets")
                .arg(self.config.native_frontend())
                .current_dir(&self.config.state)
                .env_clear()
                .envs(environment)
                .env("LES_INTERNAL_MODEL_TOKEN", control::random_id()?)
                .env("LES_INTERNAL_INGEST_TOKEN", control::random_id()?);
            if let Some(storage) = &self.config.edge_storage {
                api.arg("--edge-storage").arg(storage);
            }
            let service =
                Service::spawn("api", &mut api, self.log("logs/api-native-console.txt")?)?;
            let pid = service.child.id();
            self.services.push(service);
            journal.event("service_started", json!({"service":"api","child_pid":pid}))?;
            let deadline = Instant::now() + Duration::from_secs(75);
            loop {
                if control::stopped() {
                    return Err(Failure::new("START_CANCELLED", "Запуск отменён."));
                }
                if self.exited()?.is_some() {
                    break;
                }
                if self.probe.instance(&self.api_probe(), &self.instance)
                    && self.probe.ui(&self.ui_url())
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
                service.name == "api"
                    && service.child.try_wait().ok().flatten().is_some()
                    && !port_free(self.api)
            });
            self.stop(journal);
            if !collision || attempt == 2 {
                return Err(Failure::new(
                    "SERVICE_START_FAILED",
                    "Служба Леса завершилась при запуске. Откройте журнал запуска.",
                ));
            }
            self.api = 0;
            journal.event(
                "port_collision",
                json!({"service":"api","attempt":attempt+1}),
            )?;
        }
        unreachable!()
    }
    fn api_probe(&self) -> String {
        format!("http://127.0.0.1:{}/api/light/instance", self.api)
    }
    pub fn ui_url(&self) -> String {
        format!("http://127.0.0.1:{}/app/", self.api)
    }
    pub fn ready_fields(&self) -> Value {
        let pid = |name| {
            self.services
                .iter()
                .find(|service| service.name == name)
                .map(|service| service.child.id())
        };
        json!({"instance_id":self.instance,"ui_url":self.ui_url(),"api_url":format!("http://127.0.0.1:{}",self.api),
            "qdrant_pid":pid("qdrant"),"api_pid":pid("api"),"ui_pid":null,
            "legacy_api_pid":null,"api_native":true,
            "native_only":true,
            "vector_store":if self.config.edge_storage.is_some(){"edge"}else{"server"}})
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
        let ui = self.ui_url();
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
