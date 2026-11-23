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

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;

pub fn resolve_host(host: &str) -> Result<IpAddr> {
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

/// How long a network search for consoles waits for answers.
pub const DISCOVER_WAIT: Duration = Duration::from_secs(2);

/// Finds the address of the paired console.
///
/// - `host` set: it is used, unless the console has a known MAC (`host_id`), doesn't answer
///   there and answers at another address (its IP changed, e.g. via DHCP).
/// - `host` empty: the console is searched on the network by its MAC, or the only PS5 found is used.
pub fn resolve_console(console: &config::ConsoleConfig) -> Result<IpAddr> {
    let host = console.host.trim();
    if !host.is_empty() {
        let ip = resolve_host(host)?;
        if console.host_id.trim().is_empty() || regist::probe(ip).is_ok() {
            return Ok(ip);
        }
        return match regist::find_by_host_id(&console.host_id, DISCOVER_WAIT) {
            Ok(Some(found)) if found.ip != ip => {
                log::info!("{} moved from {ip} to {}", found.name, found.ip);
                Ok(found.ip)
            }
            _ => Ok(ip),
        };
    }
    if !console.host_id.trim().is_empty() {
        return regist::find_by_host_id(&console.host_id, DISCOVER_WAIT)?
            .map(|c| c.ip)
            .context("the paired PS5 was not found on the network (is it on or in rest mode?)");
    }
    let found = regist::discover(DISCOVER_WAIT)?;
    match found.as_slice() {
        [one] => Ok(one.ip),
        [] => bail!("no PS5 found on the network; enter its IP address"),
        _ => bail!(
            "several consoles found ({}); enter the IP address of yours",
            describe_consoles(&found)
        ),
    }
}

/// "PS5-123 (192.168.1.50, on), ..." for messages.
pub fn describe_consoles(found: &[regist::FoundConsole]) -> String {
    found
        .iter()
        .map(|c| {
            format!(
                "{} ({}, {})",
                c.name,
                c.ip,
                if c.awake { "on" } else { "rest mode" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Picks the console to pair with: only running consoles can show the Link Device code.
pub fn pick_for_pairing(found: &[regist::FoundConsole]) -> Result<&regist::FoundConsole> {
    let awake: Vec<_> = found.iter().filter(|c| c.awake).collect();
    match (awake.as_slice(), found) {
        ([one], _) => Ok(one),
        ([], []) => bail!(
            "no PS5 found on the network; is it on and in the same network? Otherwise enter its IP address"
        ),
        ([], _) => bail!(
            "the PS5 is in rest mode ({}); turn it on and open the Link Device screen",
            describe_consoles(found)
        ),
        _ => bail!(
            "several consoles are on ({}); choose one by its IP address",
            describe_consoles(found)
        ),
    }
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

    fn console(name: &str, ip: &str, awake: bool) -> regist::FoundConsole {
        regist::FoundConsole {
            ip: ip.parse().unwrap(),
            name: name.into(),
            awake,
            host_id: String::new(),
        }
    }

    #[test]
    fn pairing_picks_the_only_running_console() {
        let found = [
            console("A", "10.0.0.2", false),
            console("B", "10.0.0.3", true),
        ];
        assert_eq!(pick_for_pairing(&found).unwrap().name, "B");
        assert!(pick_for_pairing(&[]).is_err());
        assert!(pick_for_pairing(&found[..1]).is_err());
        let two_on = [
            console("A", "10.0.0.2", true),
            console("B", "10.0.0.3", true),
        ];
        assert!(pick_for_pairing(&two_on).is_err());
    }

    #[test]
    fn manual_host_without_mac_is_used_as_is() {
        let c = config::ConsoleConfig {
            host: "10.0.0.5".into(),
            ..Default::default()
        };
        assert_eq!(resolve_console(&c).unwrap().to_string(), "10.0.0.5");
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
