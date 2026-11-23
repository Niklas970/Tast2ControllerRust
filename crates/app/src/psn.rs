//! Looking up the PSN account id, like chiaki-ng's `scripts/psn-account-id.py`:
//! the user signs in on Sony's own page in their own browser, then hands us the address of
//! the "redirect" page, whose one-time `code` we exchange with Sony for the numeric account id.
//!
//! Safety rules kept here:
//! - the password is only ever typed into Sony's site in the user's browser, never into us
//! - only a URL on Sony's exact redirect address is accepted
//! - the code and the access token are used once, never logged, never stored
//! - requests go only to Sony over HTTPS with certificate verification (rustls)
//!
//! The client id / secret are the public ones of the official PS Remote Play app,
//! the same chiaki-ng uses.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

const CLIENT_ID: &str = "ba495a24-818c-472b-b12d-ff231c1b5745";
const CLIENT_SECRET: &str = "mvaiZkRsAsI1IBkY";
const REDIRECT_URI: &str = "https://remoteplay.dl.playstation.net/remoteplay/redirect";
const TOKEN_URL: &str = "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/token";

pub const LOGIN_URL: &str = concat!(
    "https://auth.api.sonyentertainmentnetwork.com/2.0/oauth/authorize",
    "?service_entity=urn:service-entity:psn&response_type=code",
    "&client_id=ba495a24-818c-472b-b12d-ff231c1b5745",
    "&redirect_uri=https://remoteplay.dl.playstation.net/remoteplay/redirect",
    "&scope=psn:clientapp&request_locale=en_US&ui=pr&service_logo=ps&layout_type=popup",
    "&smcid=remoteplay&prompt=always&PlatformPrivacyWs1=minimal&"
);

/// The account id as chiaki-ng shows it (base64 of the little endian u64) and as a number.
pub struct AccountId {
    pub base64: String,
    pub number: u64,
}

impl AccountId {
    pub fn from_number(number: u64) -> Self {
        Self {
            base64: B64.encode(number.to_le_bytes()),
            number,
        }
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Whether `text` looks like the redirect address (used to spot it on the clipboard).
#[allow(dead_code)] // used by the settings window
pub fn looks_like_redirect(text: &str) -> bool {
    code_from_redirect(text).is_ok()
}

/// Extracts the one-time code from the address of Sony's "redirect" page.
/// Anything that isn't exactly that address is rejected.
pub fn code_from_redirect(url: &str) -> Result<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix(REDIRECT_URI)
        .ok_or_else(|| anyhow!("that is not the address of Sony's \"redirect\" page (it must start with {REDIRECT_URI})"))?;
    let query = rest
        .strip_prefix('?')
        .ok_or_else(|| anyhow!("the address has no code in it; copy the whole address"))?;
    let query = query.split('#').next().unwrap_or("");
    let code = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "code")
        .and_then(|(_, v)| percent_decode(v))
        .filter(|c| !c.is_empty())
        .ok_or_else(|| anyhow!("the address has no code in it; copy the whole address"))?;
    if code.len() > 512
        || !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-._~".contains(c))
    {
        bail!("the code in the address looks wrong");
    }
    Ok(code)
}

fn sony_error(e: ureq::Error, what: &str) -> anyhow::Error {
    match e {
        ureq::Error::Status(status, resp) => {
            // show Sony's short error name only, never the raw body
            let name = resp
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .filter(|n| {
                    n.len() < 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                });
            match name.as_deref() {
                Some("invalid_grant") => {
                    anyhow!("Sony did not accept the code (it works once and expires after a few minutes); sign in again")
                }
                Some(n) => anyhow!("{what}: Sony answered {status} ({n})"),
                None => anyhow!("{what}: Sony answered {status}"),
            }
        }
        ureq::Error::Transport(t) => anyhow!("{what}: could not reach Sony ({})", t.kind()),
    }
}

/// Exchanges the one-time code for the account id.
pub fn account_id_from_code(code: &str) -> Result<AccountId> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let basic = format!(
        "Basic {}",
        B64.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    );

    let token: serde_json::Value = agent
        .post(TOKEN_URL)
        .set("Authorization", &basic)
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
        ])
        .map_err(|e| sony_error(e, "signing in"))?
        .into_json()
        .context("reading Sony's answer")?;
    let access_token = token
        .get("access_token")
        .and_then(|t| t.as_str())
        .context("Sony's answer has no access token")?;

    let encoded: String = access_token
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let info: serde_json::Value = agent
        .get(&format!("{TOKEN_URL}/{encoded}"))
        .set("Authorization", &basic)
        .call()
        .map_err(|e| sony_error(e, "reading the account"))?
        .into_json()
        .context("reading Sony's answer")?;
    let user_id = info
        .get("user_id")
        .context("Sony's answer has no account id")?;
    let number = match user_id {
        serde_json::Value::String(s) => s.parse::<u64>().ok(),
        serde_json::Value::Number(n) => n.as_u64(),
        _ => None,
    }
    .context("Sony's account id is not a number")?;
    Ok(AccountId::from_number(number))
}

/// Opens the sign-in page in the default browser.
#[cfg(windows)]
pub fn open_login_page() -> Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (verb, url) = (wide("open"), wide(LOGIN_URL));
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if r as isize <= 32 {
        bail!("could not open the browser; open this address yourself:\n{LOGIN_URL}");
    }
    Ok(())
}

/// Clipboard helpers, so the user only has to copy the address bar.
#[cfg(windows)]
pub mod clipboard {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;

    pub fn text() -> Option<String> {
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) == 0 {
                return None;
            }
            let mut out = None;
            let h = GetClipboardData(CF_UNICODETEXT);
            if !h.is_null() {
                let p = GlobalLock(h) as *const u16;
                if !p.is_null() {
                    let mut len = 0;
                    while *p.add(len) != 0 && len < 8192 {
                        len += 1;
                    }
                    out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                    GlobalUnlock(h);
                }
            }
            CloseClipboard();
            out
        }
    }

    /// Empties the clipboard (used after taking the redirect address off it).
    pub fn clear() {
        unsafe {
            if OpenClipboard(std::ptr::null_mut()) != 0 {
                EmptyClipboard();
                CloseClipboard();
            }
        }
    }
}

#[cfg(not(windows))]
pub fn open_login_page() -> Result<()> {
    bail!("open this address in your browser:\n{LOGIN_URL}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_parsing() {
        let ok = "https://remoteplay.dl.playstation.net/remoteplay/redirect?code=Ab3dE9&cid=71fac82f-5477";
        assert_eq!(code_from_redirect(ok).unwrap(), "Ab3dE9");
        assert_eq!(
            code_from_redirect(&format!("  {ok}\r\n")).unwrap(),
            "Ab3dE9"
        );
        assert_eq!(
            code_from_redirect(
                "https://remoteplay.dl.playstation.net/remoteplay/redirect?cid=x&code=Zz%2D1"
            )
            .unwrap(),
            "Zz-1"
        );
        // look-alikes and junk are refused
        for bad in [
            "http://remoteplay.dl.playstation.net/remoteplay/redirect?code=abc",
            "https://remoteplay.dl.playstation.net.evil.example/remoteplay/redirect?code=abc",
            "https://evil.example/?u=https://remoteplay.dl.playstation.net/remoteplay/redirect?code=abc",
            "https://remoteplay.dl.playstation.net/remoteplay/redirect",
            "https://remoteplay.dl.playstation.net/remoteplay/redirect?code=",
            "https://remoteplay.dl.playstation.net/remoteplay/redirect?code=a%20b",
            "Ab3dE9",
        ] {
            assert!(code_from_redirect(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn account_id_encoding_matches_chiaki() {
        // base64 of the little endian u64, as chiaki-ng's script prints it
        let a = AccountId::from_number(1);
        assert_eq!(a.base64, "AQAAAAAAAAA=");
    }
}

#[cfg(test)]
mod network_tests {
    /// Talks to Sony; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn sony_rejects_a_made_up_code() {
        let err = super::account_id_from_code("notARealCode123")
            .err()
            .expect("a made-up code must fail");
        let msg = format!("{err:#}");
        println!("error shown to the user: {msg}");
        assert!(!msg.contains("access_token"));
    }
}
