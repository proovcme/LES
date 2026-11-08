mod cli;
mod config;
mod control;
mod desktop_control;
mod journal;
mod probe;
mod services;
mod skill_runtime;

use config::{Config, Failure, Result};
use journal::Journal;
use serde_json::json;
use services::Stack;
use std::{
    fs,
    time::{Duration, Instant},
};

fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("rundll32.exe");
        command.args(["url.dll,FileProtocolHandler", url]);
        les_core::prepare_child(&mut command);
        let _ = command.spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}

fn reopen(config: &Config) -> bool {
    use std::io::Read;
    let Ok(file) = fs::File::open(config.state.join("launcher-status.json")) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(65_537).read_to_end(&mut bytes).is_err() || bytes.len() > 65_536 {
        return false;
    }
    let Some(value) = serde_json::from_slice::<serde_json::Value>(&bytes).ok() else {
        return false;
    };
    if value["phase"] != "ready" {
        return false;
    }
    let Some((api, ui, instance)) = probe::owned_addresses(&value) else {
        return false;
    };
    let Ok(probe) = probe::Probe::new() else {
        return false;
    };
    if !probe.instance(
        &format!("{}/api/light/instance", api.as_str().trim_end_matches('/')),
        &instance,
    ) {
        return false;
    }
    open_browser(ui.as_str());
    true
}

fn run(config: Config) -> Result<i32> {
    fs::create_dir_all(&config.state)
        .map_err(|_| Failure::new("STATE_UNAVAILABLE", "Нет доступа к папке данных Леса."))?;
    let Some(_owner) = control::owner(&config.state.join("launcher.lock"))? else {
        return Ok(if config.browser && reopen(&config) {
            0
        } else {
            10
        });
    };
    for name in ["data", "storage", "logs", "RAG_Content", "artifacts"] {
        fs::create_dir_all(config.state.join(name)).map_err(|_| {
            Failure::new(
                "STATE_UNAVAILABLE",
                "Не удалось подготовить папку данных Леса.",
            )
        })?;
    }
    let journal = Journal::new(&config.state, control::random_id()?);
    journal.event("launcher_started", json!({"engine":"rust"}))?;
    let panic_state = config.state.clone();
    std::panic::set_hook(Box::new(move |_| {
        let journal = Journal::new(&panic_state, String::new());
        let _ = journal.event("launcher_panic", json!({"reason":"rust_panic"}));
        let _ = journal.status(
            "error",
            "Служба запуска завершилась с ошибкой. Откройте журнал запуска.",
            json!({"reason":"rust_panic"}),
        );
    }));
    control::install(config.parent_pipe);
    let browser = config.browser;
    let mut stack = Stack::new(config)?;
    let result = (|| -> Result<()> {
        let mut recoveries: Vec<Instant> = Vec::new();
        let mut opened = String::new();
        while !control::stopped() {
            journal.status("starting", "Запускаю LES RAG…", json!({}))?;
            let url = match stack.start(&journal) {
                Ok(url) => url,
                Err(error) if error.code == "START_CANCELLED" => break,
                Err(error) => return Err(error),
            };
            journal.status("ready", "LES RAG готов", stack.ready_fields())?;
            journal.event("ready", stack.ready_fields())?;
            if browser && opened != url {
                open_browser(&url);
                opened = url;
            }
            let failure = loop {
                control::pause(Duration::from_secs(1));
                if control::stopped() {
                    break None;
                }
                if let Some(failure) = stack.failed()? {
                    break Some(failure);
                }
            };
            let Some(failure) = failure else {
                break;
            };
            journal.event("recovery_requested", failure.clone())?;
            journal.status(
                "recovering",
                "Служба остановилась или перестала отвечать. Восстанавливаю приложение…",
                json!({"failure":failure}),
            )?;
            stack.stop(&journal);
            recoveries.retain(|then| then.elapsed() < Duration::from_secs(300));
            if recoveries.len() >= 3 {
                return Err(Failure::new("RECOVERY_LIMIT","Служба повторно завершилась. Откройте журнал и повторите запуск после устранения причины."));
            }
            recoveries.push(Instant::now());
            control::pause(Duration::from_secs(2));
        }
        journal.status(
            "stopping",
            "Закрываю LES RAG…",
            json!({"reason":control::stop_reason()}),
        )?;
        Ok(())
    })();
    stack.stop(&journal);
    match result {
        Ok(()) => {
            journal.event(
                "launcher_stopped",
                json!({"reason":control::stop_reason(),"exit_code":0}),
            )?;
            journal.status(
                "stopped",
                "LES RAG остановлен",
                json!({"reason":control::stop_reason(),"exit_code":0}),
            )?;
            Ok(0)
        }
        Err(error) => {
            let _ = journal.event(
                "launcher_stopped",
                json!({"reason":error.code,"exit_code":1}),
            );
            let _ = journal.status(
                "error",
                error.message,
                json!({"reason":error.code,"exit_code":1}),
            );
            Err(error)
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(arguments) = args.strip_prefix(&["--cli".to_owned()]) {
        std::process::exit(cli::run(arguments));
    }
    if args == ["--contract"] {
        println!(
            "{}",
            json!({"schema":"les.native-runtime.v1","engine":"rust","python_core_remaining":true,
            "services":["qdrant","api","ui"],"parent_pipe":true,"state_lock":"file-exclusive",
            "native_cli":true,"native_mcp_stdio":true})
        );
        return;
    }
    let result = Config::parse(args).and_then(run);
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("{}: {}", error.code, error.message);
            std::process::exit(1);
        }
    }
}
