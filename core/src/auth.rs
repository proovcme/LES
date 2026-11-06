//! Read the existing key registry, without creating or changing it.
use chrono::{Local, NaiveDate, NaiveDateTime};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{path::Path, time::Duration};

pub enum Key {
    Missing,
    Expired,
    Role { name: String, protected: bool },
}

#[derive(Debug)]
pub enum Failure {
    Storage,
    InvalidExpiry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Actor {
    User,
    AdminKey,
    ProtectedKey,
    TrustedAdmin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Denial {
    Required,
    MissingKey,
    ExpiredKey,
    AdminRequired,
    UserRequired,
}

pub fn authorize(key: Option<Key>, trusted: Option<&str>, admin: bool) -> Result<Actor, Denial> {
    let (role, denial) = match key {
        None => (trusted, Denial::Required),
        Some(Key::Missing) => (trusted, Denial::MissingKey),
        Some(Key::Expired) => (trusted, Denial::ExpiredKey),
        Some(Key::Role { name, protected }) => {
            return role_actor(
                &name,
                admin,
                if protected {
                    Actor::ProtectedKey
                } else {
                    Actor::AdminKey
                },
            );
        }
    };
    role_actor(role.ok_or(denial)?, admin, Actor::TrustedAdmin)
}

fn role_actor(role: &str, admin: bool, administrator: Actor) -> Result<Actor, Denial> {
    match (role, admin) {
        ("admin", _) => Ok(administrator),
        ("user", false) => Ok(Actor::User),
        (_, true) => Err(Denial::AdminRequired),
        _ => Err(Denial::UserRequired),
    }
}

pub fn active_key(database: &Path, key: &str) -> Result<Key, Failure> {
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| Failure::Storage)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(|_| Failure::Storage)?;
    active_key_in(&connection, key)
}

pub(crate) fn active_key_in(connection: &Connection, key: &str) -> Result<Key, Failure> {
    let row: Option<(Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT role, expires_at FROM auth_keys WHERE key_value=? AND is_active=1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| Failure::Storage)?;
    let Some((role, expiry)) = row else {
        return Ok(Key::Missing);
    };
    let protected = is_protected(key);
    if !protected && is_expired(expiry.as_deref())? {
        return Ok(Key::Expired);
    }
    Ok(Key::Role {
        name: if protected {
            "admin".into()
        } else {
            role.filter(|value| !value.is_empty())
                .unwrap_or_else(|| "user".into())
        },
        protected,
    })
}

pub(crate) fn is_protected(key: &str) -> bool {
    trim(key).starts_with("les-admin-")
}

pub fn trim(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

pub(crate) fn is_expired(expiry: Option<&str>) -> Result<bool, Failure> {
    if let Some(expiry) = expiry.filter(|value| !value.is_empty()) {
        let expiry = expiry.replacen(' ', "T", 1);
        let parsed = NaiveDateTime::parse_from_str(&expiry, "%Y-%m-%dT%H:%M:%S%.f")
            .or_else(|_| NaiveDateTime::parse_from_str(&expiry, "%Y-%m-%dT%H:%M"))
            .or_else(|_| {
                NaiveDate::parse_from_str(&expiry, "%Y-%m-%d")
                    .map(|date| date.and_hms_opt(0, 0, 0).unwrap())
            })
            .map_err(|_| Failure::InvalidExpiry)?;
        return Ok(Local::now().naive_local() > parsed);
    }
    Ok(false)
}
