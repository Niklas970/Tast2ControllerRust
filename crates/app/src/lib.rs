//! Library of the Tast2ControllerRust app: config, key mapping, PSN login and (on Windows) input capture.
// Several APIs are only used by the settings window.
#![allow(dead_code)]

pub mod cli;
pub mod config;
pub mod input;
pub mod psn;
#[cfg(windows)]
pub mod runner;
pub mod update;

pub use tast2controller_core::{controller, http, logbuf, regist, session, stream};

use anyhow::{Context, Result};
use base64::Engine;

pub fn resolve_host(host: &str) -> Result<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    if let Ok(ip) = host.trim().parse() {
        return Ok(ip);
    }
    (host.trim(), 0)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}"))?
        .find(|a| a.is_ipv4())
        .map(|a| a.ip())
        .with_context(|| format!("no IPv4 address for {host}"))
}

pub fn parse_account_id(s: &str) -> Result<[u8; 8]> {
    if let Ok(n) = s.trim().parse::<u64>() {
        return Ok(n.to_le_bytes());
    }
    let b = base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .context("account id is neither a number nor base64")?;
    b.try_into()
        .map_err(|_| anyhow::anyhow!("account id must decode to 8 bytes"))
}

/// Sets `key = "value"` inside the `[console]` table of a TOML document, keeping comments.
pub fn set_console_value(doc: &str, key: &str, value: &str) -> String {
    let line_new = format!("{key} = \"{value}\"");
    let mut out = Vec::new();
    let mut in_console = false;
    let mut done = false;
    let mut console_header_index = None;
    for line in doc.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if in_console && !done {
                // key missing in [console]: append before the next table
                out.push(line_new.clone());
                done = true;
            }
            in_console = t == "[console]";
            if in_console {
                console_header_index = Some(out.len());
            }
        } else if in_console && !done {
            let k = t.trim_start_matches('#').trim();
            if k.split('=').next().map(str::trim) == Some(key)
                && (t.starts_with(key) || t.starts_with('#'))
            {
                out.push(line_new.clone());
                done = true;
                continue;
            }
        }
        out.push(line.to_string());
    }
    if !done {
        if in_console {
            out.push(line_new);
        } else if console_header_index.is_none() {
            out.insert(0, String::new());
            out.insert(0, line_new);
            out.insert(0, "[console]".to_string());
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn default_config_parses_and_maps() {
        let cfg: Config = toml::from_str(config::DEFAULT_CONFIG).unwrap();
        input::mapper::Mapper::new(&cfg).unwrap();
    }

    #[test]
    fn set_console_value_replaces_and_inserts() {
        let doc = config::DEFAULT_CONFIG;
        let d = set_console_value(doc, "host", "10.0.0.5");
        let d = set_console_value(&d, "regist_key", "abcd");
        let d = set_console_value(&d, "rp_key", "00112233445566778899aabbccddeeff");
        let d = set_console_value(&d, "nickname", "PS5-123");
        let cfg: Config = toml::from_str(&d).unwrap();
        assert_eq!(cfg.console.host, "10.0.0.5");
        assert_eq!(cfg.console.regist_key, "abcd");
        assert_eq!(cfg.console.nickname, "PS5-123");
        assert!(cfg.credentials().is_ok());
        // idempotent
        let d2 = set_console_value(&d, "host", "10.0.0.6");
        assert_eq!(d2.matches("host =").count(), 1);
    }

    #[test]
    fn account_id_formats() {
        let n: u64 = 1234567890123456789;
        let b64 = base64::engine::general_purpose::STANDARD.encode(n.to_le_bytes());
        assert_eq!(
            parse_account_id(&n.to_string()).unwrap(),
            parse_account_id(&b64).unwrap()
        );
    }
}
