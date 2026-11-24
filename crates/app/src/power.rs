//! Turning the paired PS5 on and off, and asking whether it is on. Used by the command line
//! and the settings window.

use std::net::IpAddr;
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::config::Config;
use crate::{regist, resolve_console, session};

/// What the console answered when asked for its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    On,
    Rest,
}

impl Power {
    /// Reads a status line such as "200 Ok" or "620 Server Standby".
    pub fn from_status(status: &str) -> Power {
        if status.trim_start().starts_with("200") {
            Power::On
        } else {
            Power::Rest
        }
    }
}

/// Asks the paired console whether it is on. Errors when it can't be found or doesn't answer.
pub fn status(cfg: &Config) -> Result<(IpAddr, Power)> {
    let ip = resolve_console(&cfg.console)?;
    Ok((ip, Power::from_status(&regist::probe(ip)?)))
}

/// Sends a wakeup to the paired console (it needs "turn on from network" enabled).
pub fn wake(cfg: &Config) -> Result<IpAddr> {
    let creds = cfg.credentials()?;
    let ip = resolve_console(&cfg.console)?;
    regist::wakeup(ip, &creds)?;
    Ok(ip)
}

/// Puts the paired console into rest mode. It must be on and not in a session of ours.
pub fn sleep(cfg: &Config) -> Result<IpAddr> {
    let creds = cfg.credentials()?;
    let ip = resolve_console(&cfg.console)?;
    let nonce = session::request_session(ip, &creds)?;
    let ctrl = session::Ctrl::connect(ip, &nonce, &creds, cfg.console.login_pin.as_deref(), false)?;
    ctrl.goto_bed()?;
    thread::sleep(Duration::from_millis(500)); // let the request go out before closing
    Ok(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        assert_eq!(Power::from_status("200 Ok"), Power::On);
        assert_eq!(Power::from_status("620 Server Standby"), Power::Rest);
    }
}
