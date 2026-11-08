use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Read, Write},
    path::Path,
};
fn run() -> les_vector::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [action, source, target] if action == "import" => {
            let result =
                les_vector::migration::import(Path::new(source), Path::new(target), |v| {
                    println!("{v}");
                    let _ = io::stdout().flush();
                })?;
            println!("{result}");
            Ok(())
        }
        [action, path] if action == "serve-stdio" => {
            let shard = les_vector::Shard::open(Path::new(path))?;
            let mut input = io::stdin().lock();
            loop {
                let mut bytes = Vec::new();
                let n = (&mut input)
                    .take(32 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut bytes)
                    .map_err(|_| "EDGE_PIPE_FAILED")?;
                if n == 0 {
                    return Ok(());
                }
                if n > 32 * 1024 * 1024 || !bytes.ends_with(b"\n") {
                    return Err("EDGE_REQUEST_TOO_LARGE");
                }
                let request: Value =
                    serde_json::from_slice(&bytes).map_err(|_| "EDGE_REQUEST_INVALID")?;
                let result = shard.call(request["action"].as_str().unwrap_or(""), &request["body"]);
                let response = match result {
                    Ok(result) => json!({"id":request["id"],"result":result}),
                    Err(code) => json!({"id":request["id"],"error":code}),
                };
                println!("{response}");
                io::stdout().flush().map_err(|_| "EDGE_PIPE_FAILED")?;
            }
        }
        _ => Err("Usage: les-vector import ABS_MANIFEST ABS_TARGET | serve-stdio ABS_TARGET"),
    }
}
fn main() {
    if let Err(code) = run() {
        eprintln!("{}", json!({"event":"edge_failed","code":code}));
        std::process::exit(1);
    }
}
