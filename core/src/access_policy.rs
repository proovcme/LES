//! Native desktop authorization settings. Never replace unreadable policy with defaults.
use regex::Regex;
use std::{collections::BTreeMap, io::Read, path::Path};

const KEYS: &[&str] = &[
    "TRUSTED_NETWORKS",
    "TRUSTED_NETWORK_ROLE",
    "TRUSTED_PROXY_NETWORKS",
    "TRUSTED_PROXY_HEADER",
    "CORS_ALLOWED_ORIGINS",
    "CORS_ALLOWED_ORIGIN_REGEX",
];
const DEFAULT_ORIGINS: &str = "http://localhost:8080,http://127.0.0.1:8080,http://localhost:8050,http://127.0.0.1:8050,http://localhost:8051,http://127.0.0.1:8051";
const DEFAULT_PATTERN: &str = r"^https?://(localhost|127\.0\.0\.1|10\.195\.146\.\d{1,3})(:\d+)?$";

pub struct Policy {
    networks: Vec<String>,
    proxy_networks: Vec<String>,
    role: String,
    pub proxy_header: String,
    origins: Vec<String>,
    pattern: Option<Regex>,
}

fn value(raw: &str) -> Result<String, &'static str> {
    // Expansion/multiline policy must be made explicit. Ignoring it could grant
    // the default loopback administrator when the operator intended key-only.
    let value = decoded_value(raw)?;
    if value.contains("${") {
        return Err("AUTH_SETTINGS_EXPANSION_UNSUPPORTED");
    }
    Ok(value)
}

fn decoded_value(raw: &str) -> Result<String, &'static str> {
    let raw = raw.trim();
    if let Some(quote @ ('\'' | '"')) = raw.chars().next() {
        let mut text = String::new();
        let mut chars = raw[quote.len_utf8()..].char_indices();
        while let Some((offset, ch)) = chars.next() {
            if ch == quote {
                let tail = raw[quote.len_utf8() + offset + ch.len_utf8()..].trim();
                return if tail.is_empty() || tail.starts_with('#') {
                    Ok(text)
                } else {
                    Err("AUTH_SETTINGS_INVALID")
                };
            }
            if ch == '\\' {
                if let Some((_, next)) = chars.next() {
                    if next == quote || next == '\\' || (quote == '"' && next == '\'') {
                        text.push(next);
                    } else if quote == '"'
                        && matches!(next, 'a' | 'b' | 'f' | 'n' | 'r' | 't' | 'v')
                    {
                        text.push(match next {
                            'a' => '\u{7}',
                            'b' => '\u{8}',
                            'f' => '\u{c}',
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            _ => '\u{b}',
                        });
                    } else {
                        text.push('\\');
                        text.push(next);
                    }
                } else {
                    return Err("AUTH_SETTINGS_INVALID");
                }
            } else {
                text.push(ch);
            }
        }
        return Err("AUTH_SETTINGS_INVALID");
    }
    let mut end = raw.len();
    let mut previous_space = false;
    for (offset, ch) in raw.char_indices() {
        if ch == '#' && previous_space {
            end = offset;
            break;
        }
        previous_space = ch.is_whitespace();
    }
    Ok(raw[..end].trim_end().into())
}

/// Read only these settings from the owned dotenv. Environment values win,
/// including an empty TRUSTED_NETWORKS, which deliberately disables trust.
pub fn load(
    path: &Path,
    environment: impl Fn(&str) -> Option<String>,
) -> Result<Policy, &'static str> {
    if !path.is_absolute() {
        return Err("AUTH_SETTINGS_PATH_INVALID");
    }
    let mut text = String::new();
    match std::fs::File::open(path) {
        Ok(file) => {
            file.take(8 * 1024 * 1024 + 1)
                .read_to_string(&mut text)
                .map_err(|_| "AUTH_SETTINGS_UNAVAILABLE")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err("AUTH_SETTINGS_UNAVAILABLE"),
    }
    if text.len() > 8 * 1024 * 1024 {
        return Err("AUTH_SETTINGS_LIMIT");
    }
    let mut raw_settings = BTreeMap::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        let line = line
            .strip_prefix("export")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .map(str::trim_start)
            .unwrap_or(line);
        let (key, raw) = line.split_once('=').unwrap_or((line, ""));
        let key = key.trim();
        let key = key
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .unwrap_or(key);
        if KEYS.contains(&key) && environment(key).is_none() {
            raw_settings.insert(key.to_owned(), raw.to_owned());
        }
    }
    let mut settings = raw_settings
        .into_iter()
        .map(|(key, raw)| Ok((key, value(&raw)?)))
        .collect::<Result<BTreeMap<String, String>, &'static str>>()?;
    for key in KEYS {
        if let Some(value) = environment(key) {
            settings.insert((*key).into(), value);
        }
    }
    Policy::build(&settings)
}

impl Policy {
    fn build(settings: &BTreeMap<String, String>) -> Result<Self, &'static str> {
        let get =
            |key: &str, default: &str| settings.get(key).cloned().unwrap_or_else(|| default.into());
        let list = |key: &str, default: &str| -> Vec<String> {
            get(key, default)
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        if settings.values().any(|v| v.len() > 16_384) {
            return Err("AUTH_SETTINGS_LIMIT");
        }
        let pattern = get("CORS_ALLOWED_ORIGIN_REGEX", DEFAULT_PATTERN);
        let pattern = if pattern.is_empty() {
            None
        } else {
            if pattern.len() > 4096 {
                return Err("AUTH_CORS_PATTERN_UNSUPPORTED");
            }
            Some(
                regex::RegexBuilder::new(&format!(r"\A(?:{pattern})\z"))
                    .size_limit(1024 * 1024)
                    .build()
                    .map_err(|_| "AUTH_CORS_PATTERN_UNSUPPORTED")?,
            )
        };
        Ok(Self {
            networks: list("TRUSTED_NETWORKS", "127.0.0.1/32,::1/128"),
            proxy_networks: list("TRUSTED_PROXY_NETWORKS", "127.0.0.1/32,::1/128"),
            role: get("TRUSTED_NETWORK_ROLE", "admin"),
            proxy_header: get("TRUSTED_PROXY_HEADER", "x-les-trusted-network"),
            origins: list("CORS_ALLOWED_ORIGINS", DEFAULT_ORIGINS),
            pattern,
        })
    }
    pub fn trusted_role(
        &self,
        forwarded: Option<&str>,
        real: Option<&str>,
        assertion: Option<&str>,
    ) -> Option<&str> {
        // les-api only binds explicit IPv4 loopback. No request header defines
        // its transport peer; forwarded addresses matter only for trusted proxies.
        let peer = "127.0.0.1";
        if crate::trust::proxy_asserts_network(
            peer,
            assertion.unwrap_or(""),
            &self.proxy_networks,
            &self.role,
        ) {
            return Some(&self.role);
        }
        let client = crate::trust::resolve_client_ip(peer, forwarded, real, &self.proxy_networks);
        crate::trust::network_role(&client, &self.networks, &self.role)
    }
    pub fn wildcard_cors(&self) -> bool {
        self.origins.iter().any(|v| v == "*")
    }
    pub fn allows_cors(&self, origin: &str) -> bool {
        self.wildcard_cors()
            || self.origins.iter().any(|v| v == origin)
            || self.pattern.as_ref().is_some_and(|re| re.is_match(origin))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(values: &[(&str, &str)]) -> Policy {
        Policy::build(
            &values
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        )
        .unwrap()
    }
    #[test]
    fn forwarding_and_assertion_respect_explicit_proxy_networks() {
        let direct = policy(&[]);
        assert_eq!(direct.trusted_role(None, None, None), Some("admin"));
        assert_eq!(direct.trusted_role(Some("192.0.2.1"), None, None), None);
        assert_eq!(
            direct.trusted_role(Some("192.0.2.1"), None, Some("true")),
            Some("admin")
        );
        let disabled = policy(&[("TRUSTED_NETWORKS", ""), ("TRUSTED_PROXY_NETWORKS", "")]);
        assert_eq!(
            disabled.trusted_role(Some("127.0.0.1"), None, Some("true")),
            None
        );
        let restricted = policy(&[
            ("TRUSTED_NETWORKS", "192.0.2.0/24"),
            ("TRUSTED_NETWORK_ROLE", "user"),
        ]);
        assert_eq!(
            restricted.trusted_role(Some("192.0.2.1, 127.0.0.1"), None, None),
            Some("user")
        );
        assert_eq!(restricted.trusted_role(Some("invalid"), None, None), None);
    }
    #[test]
    fn dotenv_policy_never_ignores_broken_or_expanded_values() {
        assert_eq!(value("'' # key only").unwrap(), "");
        assert_eq!(value("'' # ${comment}").unwrap(), "");
        assert_eq!(value("'a\\'b'").unwrap(), "a'b");
        assert_eq!(value("a # comment").unwrap(), "a");
        assert_eq!(value("'unterminated"), Err("AUTH_SETTINGS_INVALID"));
        assert_eq!(
            value("${DISABLED}"),
            Err("AUTH_SETTINGS_EXPANSION_UNSUPPORTED")
        );
    }
    #[test]
    fn owned_dotenv_preserves_empty_export_duplicates_and_environment_precedence() {
        let path = std::env::temp_dir().join(format!("les-policy-test-{}.env", std::process::id()));
        std::fs::write(&path, "\u{feff}export\tTRUSTED_NETWORKS='127.0.0.1/32'\n'TRUSTED_NETWORKS'='' # ${comment}\nTRUSTED_PROXY_NETWORKS=\nTRUSTED_NETWORK_ROLE=user\n").unwrap();
        let local = load(&path, |_| None).unwrap();
        assert_eq!(local.trusted_role(None, None, None), None);
        let overridden = load(&path, |key| {
            (key == "TRUSTED_NETWORKS").then(|| "127.0.0.1/32".into())
        })
        .unwrap();
        assert_eq!(overridden.trusted_role(None, None, None), Some("user"));
        std::fs::write(&path, "TRUSTED_NETWORKS='broken\n").unwrap();
        assert!(load(&path, |_| None).is_err());
        assert!(load(&path, |key| (key == "TRUSTED_NETWORKS").then(String::new)).is_ok());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn custom_cors_is_full_match_and_unsupported_regex_is_fail_closed() {
        let custom = policy(&[
            ("CORS_ALLOWED_ORIGINS", ""),
            ("CORS_ALLOWED_ORIGIN_REGEX", r"https://local\.example"),
        ]);
        assert!(custom.allows_cors("https://local.example"));
        assert!(!custom.allows_cors("https://local.example.evil"));
        assert!(!custom.allows_cors("https://local.example\n"));
        assert!(Policy::build(
            &[("CORS_ALLOWED_ORIGIN_REGEX".into(), "(?=unsupported)".into())].into()
        )
        .is_err());
    }
}
