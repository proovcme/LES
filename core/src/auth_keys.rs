//! Key management in the existing registry. Permission checks and mutations
//! share a transaction so a concurrent revocation cannot authorize a write.
use crate::auth::{self, Actor, Denial};
use chrono::{Days, Local};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

pub enum Operation {
    List,
    Create {
        key: String,
        holder: String,
        role: String,
        expires_days: i64,
    },
    Toggle {
        key: String,
        active: i64,
    },
    ResetDevice(String),
    Delete(String),
    Verify {
        key: String,
        fingerprint: String,
    },
}

#[derive(Debug)]
pub enum Failure {
    Storage,
    Access(Denial),
    BadRequest(&'static str),
    Conflict,
    RootRequired,
    TrustedAdminRequired,
    InvalidKey,
    ExpiredKey,
    WrongDevice,
}

impl From<rusqlite::Error> for Failure {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}

impl From<auth::Failure> for Failure {
    fn from(_: auth::Failure) -> Self {
        Self::Storage
    }
}

fn ensure_registry(connection: &Connection) -> Result<(), Failure> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS auth_keys (
        key_value TEXT PRIMARY KEY,
        holder_name TEXT NOT NULL DEFAULT '',
        role TEXT NOT NULL DEFAULT 'user',
        is_active INTEGER NOT NULL DEFAULT 1,
        created_at TEXT NOT NULL DEFAULT (datetime('now','localtime')),
        expires_at TEXT DEFAULT NULL,
        device_fingerprint TEXT DEFAULT NULL
    );",
    )?;
    let mut info = connection.prepare("PRAGMA table_info(auth_keys)")?;
    let columns: Vec<String> = info
        .query_map([], |row| row.get(1))?
        .collect::<Result<_, _>>()?;
    drop(info);
    for (column, migration) in [
        (
            "expires_at",
            "ALTER TABLE auth_keys ADD COLUMN expires_at TEXT DEFAULT NULL",
        ),
        (
            "device_fingerprint",
            "ALTER TABLE auth_keys ADD COLUMN device_fingerprint TEXT DEFAULT NULL",
        ),
    ] {
        if !columns.iter().any(|name| name == column) {
            connection.execute(migration, [])?;
        }
    }
    Ok(())
}

pub fn execute(
    database: &Path,
    credential: &str,
    trusted: Option<&str>,
    operation: Operation,
) -> Result<Value, Failure> {
    let verify_only = matches!(operation, Operation::Verify { .. });
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE;
    if !verify_only && credential.is_empty() {
        auth::authorize(None, trusted, true).map_err(Failure::Access)?;
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    if verify_only && !database.try_exists().map_err(|_| Failure::Storage)? {
        return Err(Failure::InvalidKey);
    }
    let mut connection = Connection::open_with_flags(database, flags)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let result = if let Operation::Verify { key, fingerprint } = operation {
        ensure_registry(&transaction)?;
        verify(&transaction, &key, &fingerprint)?
    } else {
        let key = if credential.is_empty() {
            None
        } else {
            Some(auth::active_key_in(&transaction, credential)?)
        };
        let actor = auth::authorize(key, trusted, true).map_err(Failure::Access)?;
        ensure_registry(&transaction)?;
        manage(&transaction, actor, operation)?
    };
    transaction.commit()?;
    Ok(result)
}

fn list(connection: &Connection) -> Result<Value, Failure> {
    let mut query = connection.prepare("SELECT key_value, holder_name, role, is_active, created_at, expires_at,
        CASE WHEN device_fingerprint IS NULL THEN 0 ELSE 1 END FROM auth_keys ORDER BY created_at DESC")?;
    let rows = query.query_map([], |row| {
        let key: String = row.get(0)?;
        let protected = auth::is_protected(&key);
        let role: String = if protected { "admin".into() } else { row.get(2)? };
        let expiry: Option<String> = if protected { None } else { row.get(5)? };
        let device: i64 = if protected { 0 } else { row.get(6)? };
        Ok(json!({"key_value":key,"holder_name":row.get::<_,String>(1)?,"role":role,
            "is_active":row.get::<_,i64>(3)?,"created_at":row.get::<_,String>(4)?,"expires_at":expiry,
            "device_bound":device,"protected_admin":i64::from(protected)}))
    })?.collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Array(rows))
}

fn create(
    connection: &Connection,
    actor: Actor,
    key: &str,
    holder: &str,
    role: &str,
    days: i64,
) -> Result<Value, Failure> {
    if auth::trim(key).is_empty() {
        return Err(Failure::BadRequest("key_value не может быть пустым"));
    }
    if !matches!(role, "user" | "admin") {
        return Err(Failure::BadRequest("Недопустимая роль ключа"));
    }
    let protected = auth::is_protected(key);
    let role = if protected { "admin" } else { role };
    if role == "admin" && !matches!(actor, Actor::TrustedAdmin | Actor::ProtectedKey) {
        return Err(Failure::RootRequired);
    }
    let expiry = if !protected && days > 0 {
        let now = Local::now().naive_local();
        let expiry = now
            .checked_add_days(Days::new(days as u64))
            .filter(|value| value.date() <= chrono::NaiveDate::from_ymd_opt(9999, 12, 31).unwrap())
            .ok_or(Failure::BadRequest("Недопустимый срок действия ключа"))?;
        Some(expiry.format("%Y-%m-%d %H:%M:%S").to_string())
    } else {
        None
    };
    match connection.execute(
        "INSERT INTO auth_keys (key_value, holder_name, role, expires_at) VALUES (?,?,?,?)",
        params![auth::trim(key), auth::trim(holder), role, expiry],
    ) {
        Ok(_) => Ok(json!({"status":"created","key_value":key,"role":role,"expires_at":expiry})),
        Err(error)
            if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) =>
        {
            Err(Failure::Conflict)
        }
        Err(error) => Err(error.into()),
    }
}

fn manage(connection: &Connection, actor: Actor, operation: Operation) -> Result<Value, Failure> {
    let guard = |key: &str| {
        if auth::is_protected(key) && actor != Actor::TrustedAdmin {
            Err(Failure::TrustedAdminRequired)
        } else {
            Ok(())
        }
    };
    match operation {
        Operation::List => list(connection),
        Operation::Create {
            key,
            holder,
            role,
            expires_days,
        } => create(connection, actor, &key, &holder, &role, expires_days),
        Operation::Toggle { key, active } => {
            guard(&key)?;
            connection.execute(
                "UPDATE auth_keys SET is_active=? WHERE key_value=?",
                params![active, key],
            )?;
            Ok(json!({"status":"ok","key_value":key,"is_active":active}))
        }
        Operation::ResetDevice(key) => {
            guard(&key)?;
            connection.execute(
                "UPDATE auth_keys SET device_fingerprint=NULL WHERE key_value=?",
                [&key],
            )?;
            Ok(json!({"status":"ok","key_value":key}))
        }
        Operation::Delete(key) => {
            guard(&key)?;
            connection.execute("DELETE FROM auth_keys WHERE key_value=?", [&key])?;
            Ok(json!({"status":"deleted","key_value":key}))
        }
        Operation::Verify { .. } => unreachable!("verification handled separately"),
    }
}

fn verify(connection: &Connection, key: &str, fingerprint: &str) -> Result<Value, Failure> {
    let key = auth::trim(key);
    let fingerprint = auth::trim(fingerprint);
    let row = connection.query_row(
        "SELECT role, holder_name, expires_at, device_fingerprint FROM auth_keys WHERE key_value=? AND is_active=1",
        [key],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?)),
    ).optional()?.ok_or(Failure::InvalidKey)?;
    let (role, holder, expiry, stored) = row;
    if auth::is_protected(key) {
        return Ok(json!({"role":"admin","holder":holder}));
    }
    if auth::is_expired(expiry.as_deref())? {
        return Err(Failure::ExpiredKey);
    }
    if !fingerprint.is_empty() {
        match stored.as_deref().filter(|value| !value.is_empty()) {
            Some(value) if value != fingerprint => return Err(Failure::WrongDevice),
            None => {
                connection.execute(
                    "UPDATE auth_keys SET device_fingerprint=? WHERE key_value=?",
                    params![fingerprint, key],
                )?;
            }
            _ => {}
        }
    }
    Ok(json!({"role":role,"holder":holder}))
}
