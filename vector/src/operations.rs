use crate::{Result, Shard};
use qdrant_edge::*;
use serde_json::{Value, json};
// Server clients used by remaining adapters predate Edge's memory placement
// field. Preserve the effective legacy disk flag without leaking SDK-only keys.
fn server_memory(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(memory) = object.remove("memory").filter(|value| !value.is_null()) {
                object.insert("on_disk".into(), json!(memory == "cold"));
            }
            for child in object.values_mut() {
                server_memory(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                server_memory(child);
            }
        }
        _ => {}
    }
}
fn persisted_vector(value: &Value) -> Result<VectorPersisted> {
    if value.is_object() {
        return serde_json::from_value(value.clone())
            .map(VectorPersisted::Sparse)
            .map_err(|_| "EDGE_POINTS_INVALID");
    }
    if value
        .as_array()
        .is_some_and(|v| v.first().is_some_and(Value::is_array))
    {
        return serde_json::from_value(value.clone())
            .map(VectorPersisted::MultiDense)
            .map_err(|_| "EDGE_POINTS_INVALID");
    }
    serde_json::from_value(value.clone())
        .map(VectorPersisted::Dense)
        .map_err(|_| "EDGE_POINTS_INVALID")
}
fn persisted_point(value: &Value) -> Result<PointStructPersisted> {
    let vector = if let Some(named) = value["vector"].as_object() {
        VectorStructPersisted::Named(
            named
                .iter()
                .map(|(k, v)| Ok((k.clone(), persisted_vector(v)?)))
                .collect::<Result<_>>()?,
        )
    } else {
        match persisted_vector(&value["vector"])? {
            VectorPersisted::Dense(v) => VectorStructPersisted::Single(v),
            VectorPersisted::MultiDense(v) => VectorStructPersisted::MultiDense(v),
            _ => return Err("EDGE_POINTS_INVALID"),
        }
    };
    Ok(PointStructPersisted {
        id: serde_json::from_value(value["id"].clone()).map_err(|_| "EDGE_POINTS_INVALID")?,
        vector,
        payload: Some(
            serde_json::from_value(value["payload"].clone()).map_err(|_| "EDGE_POINTS_INVALID")?,
        ),
    })
}
fn filter(value: &Value) -> Result<Option<Filter>> {
    if value.is_null() {
        Ok(None)
    } else {
        serde_json::from_str(&value.to_string())
            .map(Some)
            .map_err(|_| "EDGE_FILTER_INVALID")
    }
}
fn record(row: Record) -> Result<Value> {
    let vector = row
        .vector
        .map(|v| serde_json::to_value(VectorStructPersisted::from(v)))
        .transpose()
        .map_err(|_| "EDGE_RESPONSE_INVALID")?;
    Ok(json!({"id":row.id,"payload":row.payload,"vector":vector}))
}
fn limit(value: &Value, default: usize) -> Result<usize> {
    value
        .as_u64()
        .map(|v| v as usize)
        .or_else(|| value.is_null().then_some(default))
        .filter(|n| *n > 0 && *n <= 4096)
        .ok_or("EDGE_LIMIT_INVALID")
}
fn ids(value: &Value) -> Result<Vec<PointId>> {
    if !value
        .as_array()
        .is_some_and(|v| !v.is_empty() && v.len() <= 50_000)
    {
        return Err("EDGE_IDS_INVALID");
    }
    serde_json::from_value(value.clone()).map_err(|_| "EDGE_IDS_INVALID")
}
fn threshold(value: &Value) -> Result<Option<f32>> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_f64()
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .map(Some)
        .ok_or("EDGE_REQUEST_INVALID")
}
fn scoring(body: &Value) -> Result<Option<ScoringQuery>> {
    let Some(input) = body
        .get("query")
        .or_else(|| body.get("vector"))
        .filter(|v| !v.is_null())
    else {
        return Ok(None);
    };
    if input["fusion"] == "rrf" {
        return Ok(Some(ScoringQuery::Fusion(Fusion::Rrf {
            k: 2,
            weights: None,
        })));
    }
    if input["fusion"] == "dbsf" {
        return Ok(Some(ScoringQuery::Fusion(Fusion::Dbsf)));
    }
    if !input["rrf"].is_null() {
        return Err("EDGE_QUERY_NOT_SUPPORTED");
    }
    let input = input.get("nearest").unwrap_or(input);
    let query = match persisted_vector(input)? {
        VectorPersisted::Dense(v) => {
            if v.is_empty() || v.iter().any(|v| !v.is_finite()) {
                return Err("EDGE_QUERY_INVALID");
            }
            VectorInternal::Dense(v)
        }
        VectorPersisted::Sparse(v) => VectorInternal::Sparse(v),
        VectorPersisted::MultiDense(v) => Vector::new_multi(v).map_err(|_| "EDGE_QUERY_INVALID")?.0,
    };
    Ok(Some(ScoringQuery::Vector(QueryEnum::Nearest(NamedQuery {
        query,
        using: body["using"].as_str().map(str::to_owned),
    }))))
}
fn prefetches(value: &Value, depth: usize) -> Result<Vec<Prefetch>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if depth >= 4 {
        return Err("EDGE_QUERY_LIMIT");
    }
    let list = if let Some(list) = value.as_array() {
        list.clone()
    } else {
        vec![value.clone()]
    };
    if list.len() > 8 {
        return Err("EDGE_QUERY_LIMIT");
    }
    list.iter()
        .map(|body| {
            let mut request = Prefetch::new(limit(&body["limit"], 100)?);
            request.query = scoring(body)?;
            request.filter = filter(&body["filter"])?;
            request.prefetches = prefetches(&body["prefetch"], depth + 1)?;
            request.score_threshold = threshold(&body["score_threshold"])?;
            request.params = Some(
                serde_json::from_value(
                    body.get("params")
                        .cloned()
                        .unwrap_or(json!({"hnsw_ef":128})),
                )
                .map_err(|_| "EDGE_REQUEST_INVALID")?,
            );
            Ok(request)
        })
        .collect()
}
impl Shard {
    pub fn upsert(&self, rows: &[Value]) -> Result<()> {
        if rows.is_empty() || rows.len() > 512 || rows.iter().any(|p| !p["payload"].is_object()) {
            return Err("EDGE_POINTS_INVALID");
        }
        let points = rows
            .iter()
            .map(persisted_point)
            .collect::<Result<Vec<_>>>()?;
        self.edge
            .update(UpdateOperation::PointOperation(
                PointOperations::UpsertPoints(PointInsertOperations::PointsList(points)),
            ))
            .map_err(|_| "EDGE_WRITE_FAILED")?;
        self.flush()
    }
    pub fn call(&self, action: &str, body: &Value) -> Result<Value> {
        match action {
            "hybrid_search" => self.hybrid_search(body),
            "refresh_postings" => self.refresh_postings(false, |_| {}),
            "exists" => Ok(json!({"exists":true})),
            "collection_info" => {
                let mut value = self
                    .metadata
                    .get("server_info")
                    .filter(|value| value.is_object())
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let config = self.edge.config();
                let mut optimizers = serde_json::to_value(config.optimizers())
                    .map_err(|_| "EDGE_VECTOR_CONFIG_INVALID")?;
                optimizers["default_segment_number"] =
                    json!(config.optimizers().default_segment_number.unwrap_or(0));
                // Edge has no periodic flush timer; expose that explicitly in
                // the temporary server-shaped compatibility response.
                optimizers["flush_interval_sec"] = json!(0);
                value["config"] = json!({"params":{"vectors":config.vectors,"sparse_vectors":config.sparse_vectors},
                    "hnsw_config":config.hnsw_config(),"optimizer_config":optimizers});
                server_memory(&mut value["config"]);
                let info = self.info()?;
                for key in [
                    "points_count",
                    "indexed_vectors_count",
                    "segments_count",
                    "payload_schema",
                ] {
                    value[key] = info[key].clone();
                }
                value["status"] = json!("green");
                value["optimizer_status"] = json!("ok");
                Ok(value)
            }
            "create_payload_index" => {
                let create: CreateIndex = serde_json::from_value(body.clone())
                    .map_err(|_| "EDGE_INDEX_CONFIG_INVALID")?;
                self.edge
                    .update(UpdateOperation::FieldIndexOperation(
                        FieldIndexOperations::CreateIndex(create),
                    ))
                    .map_err(|_| "EDGE_WRITE_FAILED")?;
                self.flush()?;
                Ok(json!({"status":"completed"}))
            }
            "info" => self.info(),
            "count" => Ok(
                json!({"count":self.edge.count(CountRequest{filter:filter(&body["filter"])?,exact:body["exact"].as_bool().unwrap_or(true)}).map_err(|_|"EDGE_READ_FAILED")?}),
            ),
            "scroll" => {
                let req = ScrollRequest {
                    offset: if body["offset"].is_null() {
                        None
                    } else {
                        Some(
                            serde_json::from_value(body["offset"].clone())
                                .map_err(|_| "EDGE_OFFSET_INVALID")?,
                        )
                    },
                    limit: Some(limit(&body["limit"], 128)?),
                    filter: filter(&body["filter"])?,
                    with_payload: Some(
                        serde_json::from_value(
                            body.get("with_payload").cloned().unwrap_or(json!(true)),
                        )
                        .map_err(|_| "EDGE_REQUEST_INVALID")?,
                    ),
                    with_vector: serde_json::from_value(
                        body.get("with_vector").cloned().unwrap_or(json!(false)),
                    )
                    .map_err(|_| "EDGE_REQUEST_INVALID")?,
                    order_by: None,
                };
                let (rows, next) = self.edge.scroll(req).map_err(|_| "EDGE_READ_FAILED")?;
                Ok(
                    json!({"points":rows.into_iter().map(record).collect::<Result<Vec<_>>>()?,"next_page_offset":next}),
                )
            }
            "retrieve" => {
                let mut req = RetrieveRequest::new(ids(&body["ids"])?);
                req.with_payload = Some(
                    serde_json::from_value(
                        body.get("with_payload").cloned().unwrap_or(json!(true)),
                    )
                    .map_err(|_| "EDGE_REQUEST_INVALID")?,
                );
                req.with_vector = Some(
                    serde_json::from_value(
                        body.get("with_vector").cloned().unwrap_or(json!(false)),
                    )
                    .map_err(|_| "EDGE_REQUEST_INVALID")?,
                );
                Ok(json!(
                    self.edge
                        .retrieve(req)
                        .map_err(|_| "EDGE_READ_FAILED")?
                        .into_iter()
                        .map(record)
                        .collect::<Result<Vec<_>>>()?
                ))
            }
            "query" => {
                let mut request = QueryRequest::new(limit(&body["limit"], 10)?);
                request.query = scoring(body)?;
                request.prefetches = prefetches(&body["prefetch"], 0)?;
                request.filter = filter(&body["filter"])?;
                request.offset = body["offset"].as_u64().unwrap_or(0).min(100_000) as usize;
                request.score_threshold = threshold(&body["score_threshold"])?;
                request.params = Some(
                    serde_json::from_value(
                        body.get("params")
                            .cloned()
                            .unwrap_or(json!({"hnsw_ef":128})),
                    )
                    .map_err(|_| "EDGE_REQUEST_INVALID")?,
                );
                request.with_payload = serde_json::from_value(
                    body.get("with_payload").cloned().unwrap_or(json!(true)),
                )
                .map_err(|_| "EDGE_REQUEST_INVALID")?;
                request.with_vector = serde_json::from_value(
                    body.get("with_vector").cloned().unwrap_or(json!(false)),
                )
                .map_err(|_| "EDGE_REQUEST_INVALID")?;
                let rows = self.edge.query(request).map_err(|_| "EDGE_READ_FAILED")?;
                Ok(
                    json!({"points":rows.into_iter().map(|p|json!({"id":p.id,"payload":p.payload,"score":p.score,"version":p.version,"vector":p.vector.map(VectorStructPersisted::from)})).collect::<Vec<_>>()}),
                )
            }
            "set_payload" | "overwrite_payload" | "delete_payload" | "clear_payload" => {
                if body["points"].is_null() && body["filter"].is_null() {
                    return Err("EDGE_SELECTOR_REQUIRED");
                }
                let operation = match action {
                    "set_payload" => PayloadOps::SetPayload(
                        serde_json::from_value(body.clone()).map_err(|_| "EDGE_PAYLOAD_INVALID")?,
                    ),
                    "overwrite_payload" => PayloadOps::OverwritePayload(
                        serde_json::from_value(body.clone()).map_err(|_| "EDGE_PAYLOAD_INVALID")?,
                    ),
                    "delete_payload" => PayloadOps::DeletePayload(
                        serde_json::from_value(body.clone()).map_err(|_| "EDGE_PAYLOAD_INVALID")?,
                    ),
                    _ => {
                        if let Some(filter) = filter(&body["filter"])? {
                            PayloadOps::ClearPayloadByFilter(filter)
                        } else {
                            PayloadOps::ClearPayload {
                                points: ids(&body["points"])?,
                            }
                        }
                    }
                };
                self.edge
                    .update(UpdateOperation::PayloadOperation(operation))
                    .map_err(|_| "EDGE_WRITE_FAILED")?;
                self.flush()?;
                Ok(json!({"status":"completed"}))
            }
            "upsert" => {
                self.upsert(body["points"].as_array().ok_or("EDGE_POINTS_INVALID")?)?;
                Ok(json!({"status":"completed"}))
            }
            "delete" => {
                let operation = if let Some(filter) = filter(&body["filter"])? {
                    PointOperations::DeletePointsByFilter(filter)
                } else {
                    PointOperations::DeletePoints {
                        ids: ids(&body["points"])?,
                    }
                };
                self.edge
                    .update(UpdateOperation::PointOperation(operation))
                    .map_err(|_| "EDGE_WRITE_FAILED")?;
                self.flush()?;
                Ok(json!({"status":"completed"}))
            }
            "optimize" => {
                self.optimize()?;
                Ok(json!({"status":"completed"}))
            }
            _ => Err("EDGE_OPERATION_NOT_SUPPORTED"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_vectors_do_not_use_untagged_arbitrary_precision_deserialization() {
        let input:Value=serde_json::from_str(r#"{"id":"7261e509-d120-458a-a8aa-1e00e0393435","payload":{"page":5,"text":"Лес 日本語","counter":18446744073709551615},"vector":{"dense":[0.001273489937,0.72156971693,-0.69234013557],"bm25_sparse":{"indices":[17,2087609233],"values":[0.69314718,1.09861228]}}}"#).unwrap();
        let point = persisted_point(&input).unwrap();
        let output = serde_json::to_value(point).unwrap();
        assert_eq!(output["id"], input["id"]);
        assert_eq!(output["payload"], input["payload"]);
        assert_eq!(
            output["vector"]["bm25_sparse"]["indices"],
            input["vector"]["bm25_sparse"]["indices"]
        );
        assert!((output["vector"]["dense"][0].as_f64().unwrap() - 0.001273489937).abs() < 1e-9);
    }
}
