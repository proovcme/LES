//! Own the active intake's batch ordering and document attempt tickets.
use crate::catalog;
use serde_json::{json, Value};

pub fn run(
    request: &Value,
    mut process: impl FnMut(usize, u64) -> u32,
) -> Result<Value, &'static str> {
    let call = |action, arguments| {
        let result = catalog::execute(
            &json!({"schema":"les.catalog.v1","database":request["database"],
            "action":action,"arguments":arguments}),
        );
        if result["status"] != "completed" {
            Err("INDEX_BATCH_CATALOG_UNAVAILABLE")
        } else {
            Ok(result["value"].clone())
        }
    };
    let intake = request["intake_id"].as_str();
    let dataset = request["dataset_id"].as_str();
    let (action, scope) = match (intake, dataset) {
        (Some(id), None) => {
            if call("intake_index_batch", json!({"id":id}))? != request["sources"] {
                return Err("INDEX_BATCH_SELECTION_CHANGED");
            }
            ("intake_index_attempt", json!({"id":id}))
        }
        (None, Some(dataset)) => ("index_attempt", json!({"dataset_id":dataset})),
        _ => return Err("INDEX_BATCH_INVALID"),
    };
    let sources = request["sources"]
        .as_array()
        .filter(|sources| sources.len() <= 500)
        .ok_or("INDEX_BATCH_INVALID")?;
    let mut attempted = 0;
    for (index, source) in sources.iter().enumerate() {
        let mut args = scope.clone();
        args["file_name"] = source["file_name"].clone();
        args["source_path"] = source["source_path"].clone();
        let ticket = call(action, args)?;
        let attempts = ticket.as_u64().ok_or("INDEX_BATCH_INVALID")?;
        if attempts != 0 {
            attempted += 1;
        }
        match process(index, attempts) {
            0 => {}
            1 => return Ok(json!({"attempted":attempted,"interrupted":true})),
            _ => return Err("INDEX_BATCH_ADAPTER_FAILED"),
        }
    }
    Ok(json!({"attempted":attempted,"interrupted":false}))
}
