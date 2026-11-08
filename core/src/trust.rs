//! Shared trusted-network and forwarding policy. Invalid addresses never confer trust.
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr};

fn ip(value: &str) -> Option<IpAddr> {
    let value = if let Some((address, scope)) = value.split_once('%') {
        if scope.is_empty() || scope.contains(['%', '/']) || !address.contains(':') {
            return None;
        }
        address
    } else {
        value
    };
    value.parse().ok()
}
fn address_bits(value: IpAddr) -> (u128, u32) {
    match value {
        IpAddr::V4(v) => (u32::from(v) as u128, 32),
        IpAddr::V6(v) => (u128::from(v), 128),
    }
}
fn prefix(value: Option<&str>, bits: u32) -> Option<u32> {
    let Some(value) = value else {
        return Some(bits);
    };
    if !value.is_empty() && value.bytes().all(|v| v.is_ascii_digit()) {
        return value.parse::<u32>().ok().filter(|v| *v <= bits);
    }
    if bits != 32 {
        return None;
    }
    let mask = u32::from(value.parse::<Ipv4Addr>().ok()?);
    let normalized = if mask != 0 && mask >> 24 == 0 {
        !mask
    } else {
        mask
    };
    let count = normalized.leading_ones();
    let expected = if count == 0 {
        0
    } else {
        u32::MAX << (32 - count)
    };
    (normalized == expected).then_some(count)
}
pub fn ip_in_networks(value: &str, networks: &[String]) -> bool {
    let Some(address) = ip(value) else {
        return false;
    };
    let (address, bits) = address_bits(address);
    networks.iter().any(|network| {
        let (host, mask) = match network.split_once('/') {
            Some((host, mask)) => (host, Some(mask)),
            None => (network.as_str(), None),
        };
        let Some(net) = ip(host) else {
            return false;
        };
        let (net, width) = address_bits(net);
        let Some(prefix) = prefix(mask, width) else {
            return false;
        };
        if bits != width {
            return false;
        }
        if prefix == 0 {
            return true;
        }
        address >> (bits - prefix) == net >> (bits - prefix)
    })
}
pub fn resolve_client_ip(
    peer: &str,
    forwarded: Option<&str>,
    real: Option<&str>,
    networks: &[String],
) -> String {
    if ip_in_networks(peer, networks) {
        if let Some(value) = forwarded.filter(|v| !v.is_empty()) {
            return crate::auth::trim(value.split(',').next().unwrap_or("")).into();
        }
        if let Some(value) = real.filter(|v| !v.is_empty()) {
            return crate::auth::trim(value).into();
        }
    }
    peer.into()
}
pub fn network_role<'a>(value: &str, networks: &[String], role: &'a str) -> Option<&'a str> {
    ip_in_networks(value, networks).then_some(role)
}
pub fn proxy_asserts_network(peer: &str, header: &str, networks: &[String], role: &str) -> bool {
    let value = header.to_lowercase();
    ip_in_networks(peer, networks)
        && (matches!(value.as_str(), "1" | "true" | "yes" | "on") || value == role)
}
pub(crate) fn execute(request: &Value) -> Result<Value, &'static str> {
    if request["schema"] != "les.trust.v1" {
        return Err("TRUST_SCHEMA_INVALID");
    }
    let text = |key: &str| request[key].as_str().ok_or("TRUST_ARGUMENT_INVALID");
    let networks = request["networks"]
        .as_array()
        .ok_or("TRUST_NETWORKS_INVALID")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("TRUST_NETWORKS_INVALID")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let value = match text("action")? {
        "ip_in_networks" => json!(ip_in_networks(text("ip")?, &networks)),
        "resolve_client_ip" => json!(resolve_client_ip(
            text("peer")?,
            request["forwarded"].as_str(),
            request["real"].as_str(),
            &networks
        )),
        "network_role" => json!(network_role(text("ip")?, &networks, text("role")?)),
        "proxy_asserts_network" => json!(proxy_asserts_network(
            text("peer")?,
            text("header")?,
            &networks,
            text("role")?
        )),
        _ => return Err("TRUST_ACTION_INVALID"),
    };
    Ok(json!({"value": value}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwarding_does_not_trust_a_foreign_peer() {
        let networks = vec!["127.0.0.1/32".into(), "::1/128".into()];
        assert_eq!(
            resolve_client_ip("192.0.2.1", Some("127.0.0.1"), None, &networks),
            "192.0.2.1"
        );
        assert!(!proxy_asserts_network(
            "192.0.2.1",
            "true",
            &networks,
            "admin"
        ));
        assert_eq!(
            resolve_client_ip("::1", Some(" 192.0.2.1,127.0.0.1"), None, &networks),
            "192.0.2.1"
        );
        assert!(proxy_asserts_network(
            "127.0.0.1",
            "TRUE",
            &networks,
            "admin"
        ));
    }
    #[test]
    fn invalid_masks_and_address_families_never_confer_trust() {
        for value in [
            "127.0.0.1/33",
            "127.0.0.1/255.0.255.0",
            "127.0.0.1/-1",
            "invalid",
            "::1/32",
        ] {
            assert!(!ip_in_networks("127.0.0.1", &[value.into()]));
        }
        assert!(ip_in_networks(
            "fe80::abcd%zone",
            &["fe80::1%other/64".into()]
        ));
        assert!(ip_in_networks(
            "127.1.2.3",
            &["127.0.0.1/0.255.255.255".into()]
        ));
    }
}
