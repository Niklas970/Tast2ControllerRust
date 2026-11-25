//! Self-update from GitHub Releases: find the newest release, download the EXE,
//! check it against the published SHA-256 and swap it in place of the running program.
//!
//! The EXE is not signed, so the checksum is the integrity check. Both files come from the
//! same release of this repository, and download links outside it are refused.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

pub const REPO: &str = "Niklas970/Tast2ControllerRust";
pub const EXE_ASSET: &str = "tast2controller.exe";
pub const SHA_ASSET: &str = "tast2controller.exe.sha256";
const MAX_EXE_BYTES: u64 = 200 * 1024 * 1024;
const USER_AGENT: &str = concat!("tast2controller-updater/", env!("CARGO_PKG_VERSION"));

/// The version this program was built as.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Parses `1.2.3` or `v1.2.3` (a `-suffix` is ignored).
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (
        it.next()??,
        it.next().unwrap_or(Some(0))?,
        it.next().unwrap_or(Some(0))?,
    );
    if it.next().is_some() {
        return None;
    }
    Some(v)
}

/// True when `latest` is a higher version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub exe_url: String,
    pub sha_url: String,
}

fn download_prefix() -> String {
    format!("https://github.com/{REPO}/releases/download/")
}

/// Reads the answer of `GET /repos/{REPO}/releases/latest`.
pub fn parse_release(v: &serde_json::Value) -> Result<Release> {
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .context("release has no tag")?
        .to_string();
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .context("release has no files")?;
    let url_of = |name: &str| -> Result<String> {
        let url = assets
            .iter()
            .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(name))
            .and_then(|a| a.get("browser_download_url"))
            .and_then(|u| u.as_str())
            .with_context(|| format!("release {tag} has no {name}"))?;
        if !url.starts_with(&download_prefix()) {
            bail!("release file {name} does not come from {REPO}");
        }
        Ok(url.to_string())
    };
    Ok(Release {
        exe_url: url_of(EXE_ASSET)?,
        sha_url: url_of(SHA_ASSET)?,
        tag,
    })
}

/// Reads a `sha256sum` style line (`<hex>  <name>`) and returns the hash for `name`.
pub fn parse_checksum(text: &str, name: &str) -> Result<[u8; 32]> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), file) = (parts.next(), parts.next()) else {
            continue;
        };
        if file.is_some_and(|f| f.trim_start_matches('*') != name) {
            continue;
        }
        let bytes = crate::http::parse_hex(hash).context("checksum is not hex")?;
        return bytes
            .try_into()
            .map_err(|_| anyhow!("checksum is not a SHA-256"));
    }
    bail!("no checksum for {name}")
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .user_agent(USER_AGENT)
        .build()
}

fn net_error(e: ureq::Error, what: &str) -> anyhow::Error {
    match e {
        ureq::Error::Status(404, _) => anyhow!("{what}: no release published yet"),
        ureq::Error::Status(403, _) => anyhow!("{what}: GitHub limits requests, try again later"),
        ureq::Error::Status(s, _) => anyhow!("{what}: GitHub answered {s}"),
        ureq::Error::Transport(t) => anyhow!("{what}: could not reach GitHub ({})", t.kind()),
    }
}

/// Asks GitHub for the newest published release.
pub fn latest() -> Result<Release> {
    let v: serde_json::Value = agent()
        .get(&format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| net_error(e, "checking for updates"))?
        .into_json()
        .context("reading GitHub's answer")?;
    parse_release(&v)
}

/// Asks GitHub for the newest release. `Ok(None)` means this program is up to date.
pub fn check() -> Result<Option<Release>> {
    let rel = latest()?;
    Ok(is_newer(&rel.tag, current_version()).then_some(rel))
}

/// Downloads the release EXE and returns it after the SHA-256 matched.
pub fn download(rel: &Release) -> Result<Vec<u8>> {
    let a = agent();
    let sums = a
        .get(&rel.sha_url)
        .call()
        .map_err(|e| net_error(e, "downloading the checksum"))?
        .into_string()
        .context("reading the checksum")?;
    let expected = parse_checksum(&sums, EXE_ASSET)?;
    let mut exe = Vec::new();
    a.get(&rel.exe_url)
        .call()
        .map_err(|e| net_error(e, "downloading the update"))?
        .into_reader()
        .take(MAX_EXE_BYTES + 1)
        .read_to_end(&mut exe)
        .context("downloading the update")?;
    if exe.len() as u64 > MAX_EXE_BYTES {
        bail!("the download is too large");
    }
    if sha256(&exe) != expected {
        bail!("the download does not match its SHA-256, update not installed");
    }
    Ok(exe)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Puts `new_exe` in place of `target`. The old file is kept as `<target>.old`, because
/// Windows lets a running program be renamed but not deleted or overwritten.
pub fn replace_file(target: &Path, new_exe: &[u8]) -> Result<()> {
    let staged = with_suffix(target, ".new");
    let old = with_suffix(target, ".old");
    std::fs::write(&staged, new_exe).with_context(|| format!("writing {}", staged.display()))?;
    if old.exists() && std::fs::remove_file(&old).is_err() {
        // Left over from an earlier update and still in use (an older instance is running).
        // Windows can't delete it, but it can rename it: move it out of the way under a
        // unique name; `cleanup_old` deletes it on a later start.
        let parked = leftover_name(target);
        if let Err(e) = std::fs::rename(&old, &parked) {
            let _ = std::fs::remove_file(&staged);
            return Err(e).with_context(|| {
                format!(
                    "moving {} aside (close other running copies of the program and try again)",
                    old.display()
                )
            });
        }
    }
    if let Err(e) = std::fs::rename(target, &old) {
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("moving {} aside", target.display()));
    }
    if let Err(e) = std::fs::rename(&staged, target) {
        let _ = std::fs::rename(&old, target);
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("installing {}", target.display()));
    }
    Ok(())
}

/// A free name `<target>.old-<n>` for a leftover that is still in use.
fn leftover_name(target: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    (0..)
        .map(|n| with_suffix(target, &format!(".old-{stamp}-{n}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

/// All files an earlier update left behind next to `exe` (`.old` and `.old-*`).
fn leftovers(exe: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.old", name.to_string_lossy());
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy())
                        .is_some_and(|n| n == prefix || n.starts_with(&format!("{prefix}-")))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Downloads, verifies and installs `rel` over the running program. Returns its path.
pub fn install(rel: &Release) -> Result<PathBuf> {
    let exe = download(rel)?;
    let target = std::env::current_exe().context("finding the running program")?;
    replace_file(&target, &exe)?;
    log::info!("updated to {} ({})", rel.tag, target.display());
    Ok(target)
}

/// Removes the files earlier updates left behind. The previous process may still be
/// closing, so this retries for a few seconds in the background; files still in use stay
/// until a later start.
pub fn cleanup_old() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut left = leftovers(&exe);
    if left.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..20 {
            left.retain(|p| std::fs::remove_file(p).is_err() && p.exists());
            if left.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// Starts the (updated) program again without arguments, i.e. with the settings window.
pub fn restart(exe: &Path) -> Result<()> {
    std::process::Command::new(exe)
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert_eq!(parse_version("v1.2.3-beta"), Some((1, 2, 3)));
        assert_eq!(parse_version("latest"), None);
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("nonsense", "0.1.0"));
    }

    fn release_json(exe_url: &str) -> serde_json::Value {
        serde_json::json!({
            "tag_name": "v0.2.0",
            "assets": [
                { "name": EXE_ASSET, "browser_download_url": exe_url },
                { "name": SHA_ASSET, "browser_download_url":
                    format!("{}v0.2.0/{SHA_ASSET}", download_prefix()) },
            ]
        })
    }

    #[test]
    fn release_parsing() {
        let exe = format!("{}v0.2.0/{EXE_ASSET}", download_prefix());
        let rel = parse_release(&release_json(&exe)).unwrap();
        assert_eq!(rel.tag, "v0.2.0");
        assert_eq!(rel.exe_url, exe);
        assert!(rel.sha_url.ends_with(SHA_ASSET));
        // links outside this repository are refused
        assert!(parse_release(&release_json("https://example.com/tast2controller.exe")).is_err());
        assert!(parse_release(&serde_json::json!({ "tag_name": "v1", "assets": [] })).is_err());
    }

    #[test]
    fn checksum_parsing() {
        let h = sha256(b"hello");
        let hex = crate::http::hex(&h);
        assert_eq!(
            parse_checksum(&format!("{hex}  {EXE_ASSET}\r\n"), EXE_ASSET).unwrap(),
            h
        );
        assert_eq!(
            parse_checksum(&format!("{hex} *{EXE_ASSET}"), EXE_ASSET).unwrap(),
            h
        );
        assert_eq!(parse_checksum(&hex, EXE_ASSET).unwrap(), h);
        assert!(parse_checksum(&format!("{hex}  other.exe"), EXE_ASSET).is_err());
        assert!(parse_checksum("abcd  tast2controller.exe", EXE_ASSET).is_err());
        assert_eq!(
            crate::http::hex(&sha256(b"hello")),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn replaces_file_and_keeps_old() {
        let dir = std::env::temp_dir().join(format!("t2c-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("app.exe");
        std::fs::write(&target, b"v1").unwrap();
        replace_file(&target, b"v2").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v2");
        assert_eq!(std::fs::read(dir.join("app.exe.old")).unwrap(), b"v1");
        // a second update replaces the leftover .old
        replace_file(&target, b"v3").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"v3");
        assert_eq!(std::fs::read(dir.join("app.exe.old")).unwrap(), b"v2");
        assert!(!dir.join("app.exe.new").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_leftovers_only() {
        let dir = std::env::temp_dir().join(format!("t2c-leftovers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "app.exe",
            "app.exe.old",
            "app.exe.old-1-0",
            "app.exe.new",
            "other.exe.old",
        ] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let mut names: Vec<String> = leftovers(&dir.join("app.exe"))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["app.exe.old", "app.exe.old-1-0"]);
        let parked = leftover_name(&dir.join("app.exe"));
        assert!(!parked.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    #[ignore = "needs network"]
    fn github_latest_release_downloads_and_verifies() {
        let rel = latest().unwrap();
        assert!(parse_version(&rel.tag).is_some());
        let exe = download(&rel).unwrap();
        assert_eq!(&exe[..2], b"MZ");
    }
}
