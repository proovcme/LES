//! Explicit first read of a registered companion source, never an arbitrary path.
#[cfg(test)]
mod tests;
use super::*;

impl Companion {
    pub(super) async fn registered_sources(&self, body: &Value) -> Result<Value> {
        let project = id(body, "project_id")?;
        let status = self.get("/api/models/status", &[]).await?;
        let readers = status["readers"]
            .as_object()
            .ok_or("ENGINEERING_RESPONSE_INVALID")?;
        let query = body["q"].as_str().unwrap_or("");
        if query.len() > 256 {
            return Err("ENGINEERING_REQUEST_INVALID".into());
        }
        let page = self
            .get(
                "/api/documents",
                &[
                    ("project_id", project.into()),
                    ("offset", body["offset"].as_u64().unwrap_or(0).to_string()),
                    ("q", query.into()),
                ],
            )
            .await?;
        if !page["next_offset"].is_null()
            && !page["next_offset"]
                .as_u64()
                .is_some_and(|n| n > body["offset"].as_u64().unwrap_or(0))
        {
            return Err("ENGINEERING_CURSOR_INVALID".into());
        }
        if page["project_id"] != project {
            return Err("ENGINEERING_IDENTITY_MISMATCH".into());
        }
        let mut items = Vec::new();
        for row in page["documents"]
            .as_array()
            .ok_or("ENGINEERING_RESPONSE_INVALID")?
        {
            let name = row["relative_path"]
                .as_str()
                .ok_or("ENGINEERING_RESPONSE_INVALID")?;
            let format = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            let Some(reader) = readers.get(&format) else {
                continue;
            };
            id(row, "doc_id")?;
            items.push(
                json!({"doc_id":row["doc_id"],"relative_path":name,"format":format,
                "sha256":row["sha256"],"available":reader["available"]==true,
                "missing":row["status"]=="missing" || row["status"]=="MISSING"}),
            );
        }
        // Never return the companion's transient write token to the browser or model.
        Ok(json!({"project_id":project,"items":items,"next_offset":page["next_offset"]}))
    }

    pub(super) async fn read_registered(&self, body: &Value) -> Result<String> {
        let project = id(body, "project_id")?;
        let doc = id(body, "doc_id")?;
        let request = id(body, "request_id")?;
        if request.len() > 128 {
            return Err("ENGINEERING_REQUEST_INVALID".into());
        }
        let expected = body
            .get("expected_source_sha256")
            .filter(|v| v.as_str() != Some(""));
        if let Some(value) = expected {
            les_core::engineering::digest(value).map_err(str::to_owned)?;
        }
        // Use the companion's existing local handshake only after an explicit user action.
        let status = self.get("/api/models/status", &[]).await?;
        let token = status["write_token"]
            .as_str()
            // An opaque local credential, not a SHA digest. The companion uses
            // UUID hex today; other bounded header-safe tokens remain compatible.
            .filter(|s| (16..=1024).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_graphic()))
            .ok_or("ENGINEERING_PERMISSION_DENIED")?;
        let mut url = self
            .url
            .join("/api/models/read")
            .map_err(|_| "ENGINEERING_ENDPOINT_INVALID")?;
        url.query_pairs_mut().append_pair("project_id", project);
        let mut response = self
            .client
            .post(url)
            .timeout(Duration::from_secs(150))
            .header("Origin", self.url.as_str().trim_end_matches('/'))
            .header("X-Uvyazka-Token", token)
            .header("Content-Type", "application/json")
            .body(json!({"doc_id":doc,"request_id":request}).to_string())
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    "ENGINEERING_READ_TIMEOUT"
                } else {
                    "ENGINEERING_UNAVAILABLE"
                }
            })?;
        if response.status() == reqwest::StatusCode::FORBIDDEN {
            return Err("ENGINEERING_PERMISSION_DENIED".into());
        }
        if !response.status().is_success() {
            return Err("ENGINEERING_READ_FAILED".into());
        }
        let mut bytes = Vec::new();
        while let Some(part) = response
            .chunk()
            .await
            .map_err(|_| "ENGINEERING_READ_FAILED")?
        {
            if bytes.len() + part.len() > 1024 * 1024 {
                return Err("ENGINEERING_RESPONSE_LIMIT".into());
            }
            bytes.extend_from_slice(&part);
        }
        let result: Value =
            serde_json::from_slice(&bytes).map_err(|_| "ENGINEERING_RESPONSE_INVALID")?;
        if result["project_id"] != project || result["source"]["doc_id"] != doc {
            return Err("ENGINEERING_IDENTITY_MISMATCH".into());
        }
        if result["freshness"]["status"] != "content_hash_matches"
            || expected.is_some_and(|sha| result["source"]["sha256"] != *sha)
        {
            return Err("ENGINEERING_SOURCE_CHANGED".into());
        }
        Ok(id(&result, "dataset_id")?.to_owned())
    }
}
