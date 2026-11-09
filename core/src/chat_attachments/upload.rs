//! A public upload receipt is published atomically with the original's card.
use super::*;

pub struct UploadClaim {
    pub ident: String,
    pub(super) owner: String,
    pub(super) key: String,
    pub(super) fingerprint: String,
    pub(super) sha256: String,
    pub(super) size: u64,
}
pub fn claim(
    path: &Path,
    name: &str,
    caller: &str,
    key: Option<&str>,
    kind: &str,
) -> Result<UploadClaim> {
    if key.is_some_and(|v| v.is_empty() || v.len() > 256 || v.chars().any(char::is_control)) {
        return Err("ATTACHMENT_KEY_INVALID");
    }
    let owner = crate::sha256_hex(caller.as_bytes());
    let key = key
        .map(|k| {
            crate::sha256_hex(
                json!(["les.chat-upload.v1", owner, k])
                    .to_string()
                    .as_bytes(),
            )
        })
        .unwrap_or_default();
    let ident = if key.is_empty() {
        new_id()?
    } else {
        format!("read_{}", &key[..12])
    };
    let (size, sha256) = digest(&mut File::open(path).map_err(|_| "ATTACHMENT_READ_FAILED")?)?;
    let fingerprint = crate::sha256_hex(json!([kind, name, size, sha256]).to_string().as_bytes());
    Ok(UploadClaim {
        ident,
        owner,
        key,
        fingerprint,
        sha256,
        size,
    })
}
pub(super) fn existing(base: &Path, ident: &str, claim: &UploadClaim) -> Result<Option<Value>> {
    let path = base.join(format!("{ident}.json"));
    let card = match read_card(&path) {
        Err("ATTACHMENT_NOT_FOUND") => return Ok(None),
        result => result?,
    };
    if claim.key.is_empty()
        || card["upload_key"] != claim.key
        || card["upload_owner"] != claim.owner
    {
        return Err("ATTACHMENT_ID_CONFLICT");
    }
    if card["upload_fingerprint"] != claim.fingerprint {
        return Err("ATTACHMENT_KEY_REUSED");
    }
    if card["upload_expired"] == true {
        return Err("ATTACHMENT_EXPIRED");
    }
    Ok(Some(card))
}
pub fn replay_upload(base: &Path, claim: &UploadClaim, age: u64) -> Result<Option<Value>> {
    if claim.key.is_empty() {
        return Ok(None);
    }
    let base = match root(base, false) {
        Err("ATTACHMENT_NOT_FOUND") => return Ok(None),
        v => v?,
    };
    if existing(&base, &claim.ident, claim)?.is_none() {
        return Ok(None);
    }
    let asset = resolve_for(&base, &claim.ident, age, Some(&claim.owner))?;
    Ok(Some(asset.metadata["upload_response"].clone()))
}
pub fn save_upload(
    base: &Path,
    path: &Path,
    name: &str,
    claim: &UploadClaim,
    response: &Value,
) -> Result<Value> {
    let card = preserve_inner(base, path, &claim.ident, name, Some((claim, response)))?;
    Ok(card["upload_response"].clone())
}
impl UploadClaim {
    pub fn digest(&self) -> &str {
        &self.sha256
    }
}
