//! Tiny HTTP/1.1 helpers for the Remote Play session, ctrl and registration requests.

use std::io::Read;
use std::net::TcpStream;

use anyhow::{bail, Context, Result};

pub struct Response {
    pub code: u16,
    pub headers: Vec<(String, String)>,
    /// Bytes received after the end of the header.
    pub rest: Vec<u8>,
}

impl Response {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Parses `Key: value` lines.
pub fn parse_headers(text: &str) -> Vec<(String, String)> {
    text.split(['\r', '\n'])
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            let v = v.strip_prefix(' ').unwrap_or(v);
            if k.is_empty() || v.is_empty() {
                return None;
            }
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// Reads from the stream until the end of the HTTP header and parses it.
/// The stream's read timeout bounds the wait.
pub fn recv_response(stream: &mut TcpStream) -> Result<Response> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut chunk).context("receiving HTTP response")?;
        if n == 0 {
            bail!("connection closed before the HTTP header was complete");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 16 * 1024 {
            bail!("HTTP header too large");
        }
    };
    let text = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = text.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let code: u16 = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .with_context(|| format!("invalid HTTP status line {status:?}"))?;
    let headers = parse_headers(&text[status.len()..]);
    Ok(Response {
        code,
        headers,
        rest: buf[header_end..].to_vec(),
    })
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        bail!("hex string has odd length");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("invalid hex"))
        .collect()
}
