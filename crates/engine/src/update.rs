//! Updates (Help ▸ Check for Updates…, Settings ▸ Startup & Repair; `app.update.*` commands).
//!
//! Every push to the fork's `main` publishes a GitHub Release (`.github/workflows/fork-release.yml`)
//! with `latest.json`, a manifest of the release's files (URL, size and SHA-256 per platform), and
//! `latest.json.sig`, its Ed25519 signature. The app fetches both, checks the signature against
//! [`PUBLIC_KEY`] and compares versions. On Windows, installed with the MSI, it downloads the new
//! MSI in the background, checks its size and SHA-256, and offers **Restart to Update**: the app
//! quits normally (a modified project asks to be saved first), a helper waits for it to exit, runs
//! the MSI (Windows asks for permission, as for any per-machine install) and starts the app again.
//! The portable build and other platforms are told a new version exists, with a link.
//!
//! The MSI is not code-signed; the manifest signature is what proves an update is ours. Files
//! downloaded with `curl` carry no Mark-of-the-Web, so SmartScreen doesn't stop the install.
//! Like model downloads (`models.rs`), fetching uses the system's `curl` (shipped with Windows 10+):
//! no networking code is linked in. Headless sessions (CLI, control channel, tests) never check on
//! their own: only a host that sets [`Updater::auto`] does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, b_p, bad};
use crate::{Event, Result, Session, cmd, links, query};

/// The Ed25519 public key `latest.json` is signed with (hex). Its private half is the
/// `UPDATE_SIGNING_KEY` secret of the release workflow (`cargo xtask update-keygen`, docs/updates.md).
pub const PUBLIC_KEY: &str = "0a18858b2a50d03aa53af0e3afae2d9324e437481a1846ca67f5960acb3df867";

/// The manifest's URL (`EFFECTCRAFT_UPDATE_URL` overrides it, for testing); its signature is at
/// the same URL plus `.sig`.
pub fn manifest_url() -> String {
    std::env::var("EFFECTCRAFT_UPDATE_URL").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| links::UPDATE_MANIFEST.to_string())
}

/// How often a running app checks again (and how long after the last check a launch waits).
const CHECK_EVERY_SECS: u64 = 24 * 60 * 60;
/// No release file is this big; a manifest that says otherwise is refused.
const MAX_DOWNLOAD: u64 = 2_000_000_000;

/// One downloadable file of a release.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub url: String,
    pub size: u64,
    pub sha256: String,
}

/// `latest.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// The release page (shown to builds that can't install by themselves).
    #[serde(default)]
    pub page: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub date: String,
    /// By platform: `windows-x64` (the MSI), `windows-x64-portable`, `macos-universal`…
    #[serde(default)]
    pub platforms: BTreeMap<String, Asset>,
}

/// Where the updater is.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum State {
    #[default]
    Idle,
    Checking,
    /// The newest release is this one (or older).
    UpToDate {
        latest: String,
    },
    Downloading {
        version: String,
    },
    /// Downloaded and verified: Restart to Update installs it.
    #[serde(rename_all = "camelCase")]
    Ready {
        version: String,
        notes: String,
        path: String,
    },
    /// Newer, but this build can't install it by itself (portable, macOS, Linux): `url` is the
    /// release page.
    Available {
        version: String,
        notes: String,
        url: String,
    },
    Failed {
        error: String,
    },
}

#[derive(Default)]
struct Shared {
    state: State,
    /// A finished check's message and whether it failed, for a toast (checks the user asked for).
    done: Option<(String, bool)>,
}

/// The session's updater.
#[derive(Default)]
pub struct Updater {
    shared: Arc<Mutex<Shared>>,
    /// Check at launch and once a day while running (the desktop app sets it, unless an agent
    /// drives it; Settings ▸ Startup & Repair can turn it off).
    pub auto: bool,
    last_auto: Option<web_time::Instant>,
    /// The version whose banner Later hid (it's offered again at the next launch).
    pub dismissed: Option<String>,
    /// Restart to Update was chosen: install when the app exits.
    install_on_exit: bool,
}

impl Updater {
    pub fn state(&self) -> State {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner).state.clone()
    }

    fn busy(&self) -> bool {
        matches!(self.state(), State::Checking | State::Downloading { .. })
    }

    /// The update the banner offers: ready to install, or available elsewhere, and not dismissed.
    pub fn offer(&self) -> Option<State> {
        let st = self.state();
        let version = match &st {
            State::Ready { version, .. } | State::Available { version, .. } => version,
            _ => return None,
        };
        (self.dismissed.as_deref() != Some(version.as_str())).then_some(st)
    }

    /// Called as the app exits: after Restart to Update, start the installer once this process
    /// is gone. Errors are only logged: the app is quitting either way.
    pub fn finish_on_exit(&mut self) {
        if !std::mem::take(&mut self.install_on_exit) {
            return;
        }
        let State::Ready { path, .. } = self.state() else { return };
        match launch_installer(Path::new(&path)) {
            Ok(()) => log::info!("update: installing {path} after exit"),
            Err(e) => log::warn!("update: cannot start the installer: {e}"),
        }
    }
}

/// How this build can be updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Install {
    /// Installed with the MSI (Windows): downloads and installs updates.
    Msi,
    /// The Windows portable zip (`portable.txt` beside the executable): links to the release.
    Portable,
    /// macOS, Linux, the browser: links to the release.
    Manual,
}

pub fn install_kind() -> Install {
    if !cfg!(target_os = "windows") {
        return Install::Manual;
    }
    let portable = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("portable.txt"))).is_some_and(|p| p.is_file());
    if portable { Install::Portable } else { Install::Msi }
}

/// This build's key in [`Manifest::platforms`].
pub fn platform_key(install: Install) -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "x86",
        "aarch64" => "arm64",
        a => a,
    };
    match (std::env::consts::OS, install) {
        ("windows", Install::Portable) => format!("windows-{arch}-portable"),
        ("windows", _) => format!("windows-{arch}"),
        ("macos", _) => "macos-universal".into(),
        (os, _) => format!("{os}-{arch}"),
    }
}

/// `major.minor.patch` (a `-pre` or `+build` suffix is ignored); `None` when it isn't one.
pub fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let (a, b, c) = (parts.next()??, parts.next()??, parts.next().unwrap_or(Some(0))?);
    parts.next().is_none().then_some((a, b, c))
}

/// Is `latest` newer than `current`? Unparsable versions are never newer.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn hex_bytes<const N: usize>(hex: &str) -> Option<[u8; N]> {
    let hex = hex.trim().as_bytes();
    if hex.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (o, pair) in out.iter_mut().zip(hex.chunks(2)) {
        let s = std::str::from_utf8(pair).ok()?;
        *o = u8::from_str_radix(s, 16).ok()?;
    }
    Some(out)
}

/// Check `manifest` against its signature (hex) and `public_key` (hex), then read it.
pub fn verify_manifest(manifest: &[u8], signature: &str, public_key: &str) -> std::result::Result<Manifest, String> {
    let key = hex_bytes::<32>(public_key).ok_or("this build has no valid update key")?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|_| "this build has no valid update key".to_string())?;
    let sig = hex_bytes::<64>(signature).ok_or("the update's signature is malformed")?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig);
    key.verify_strict(manifest, &sig)
        .map_err(|_| "the update's signature doesn't match: it was not published by EffectCraft's release workflow".to_string())?;
    serde_json::from_slice(manifest).map_err(|e| format!("the update manifest is malformed: {e}"))
}

/// What a check found, given the verified manifest.
pub enum Found {
    UpToDate,
    /// Download and install this.
    Install(Asset),
    /// Tell the user (`page`).
    Tell,
}

pub fn decide(m: &Manifest, current: &str, install: Install) -> std::result::Result<Found, String> {
    if !is_newer(&m.version, current) {
        return Ok(Found::UpToDate);
    }
    if install != Install::Msi {
        return Ok(Found::Tell);
    }
    let key = platform_key(install);
    let a = m.platforms.get(&key).ok_or_else(|| format!("EffectCraft {} has no {key} installer", m.version))?;
    if a.size == 0 || a.size > MAX_DOWNLOAD || hex_bytes::<32>(&a.sha256).is_none() || !a.url.starts_with("https://") {
        return Err(format!("EffectCraft {}'s {key} entry is malformed", m.version));
    }
    Ok(Found::Install(a.clone()))
}

/// The file name a release's installer is kept under (only characters safe in a path).
fn installer_name(version: &str) -> String {
    let v: String = version.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')).collect();
    format!("effectcraft-{v}-update.msi")
}

/// Is `path` the published file (size and SHA-256)?
fn verified(path: &Path, a: &Asset) -> std::result::Result<bool, String> {
    match std::fs::metadata(path) {
        Ok(md) if md.len() == a.size => {}
        _ => return Ok(false),
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(effectcraft_segment::sha256::hex(&bytes).eq_ignore_ascii_case(a.sha256.trim()))
}

/// Where downloaded installers wait (not roamed: `%LOCALAPPDATA%` on Windows).
fn updates_dir(config: Option<PathBuf>) -> PathBuf {
    let local = std::env::var_os("LOCALAPPDATA").filter(|_| cfg!(target_os = "windows")).map(|d| PathBuf::from(d).join("EffectCraft"));
    local.or(config).unwrap_or_else(std::env::temp_dir).join("Updates")
}

fn curl(url: &str) -> std::process::Command {
    let mut c = std::process::Command::new("curl");
    c.args(["-fsSL", "--retry", "2", "--connect-timeout", "20"]);
    // The real manifest is only ever fetched over HTTPS (and so are its redirects); a test
    // override may point at a local server.
    if std::env::var_os("EFFECTCRAFT_UPDATE_URL").is_none() {
        c.args(["--proto", "=https", "--proto-redir", "=https"]);
    }
    c.arg(url);
    // A console program started from the windowed app would flash a console window.
    #[cfg(target_os = "windows")]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // CREATE_NO_WINDOW
    c
}

fn fetch(url: &str) -> std::result::Result<Vec<u8>, String> {
    let mut c = curl(url);
    c.args(["--max-time", "60"]);
    let out = c.output().map_err(|e| format!("checking for updates needs curl ({e})"))?;
    if !out.status.success() {
        return Err(format!("cannot reach {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

fn download(a: &Asset, dest: &Path) -> std::result::Result<(), String> {
    let tmp = dest.with_extension("part");
    let mut c = curl(&a.url);
    c.args(["--max-time", "3600", "-o"]).arg(&tmp);
    let out = c.output().map_err(|e| format!("downloading needs curl ({e})"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("download failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    if !verified(&tmp, a)? {
        let _ = std::fs::remove_file(&tmp);
        return Err("the download doesn't match the published installer (size or SHA-256 differs)".into());
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("cannot keep the download: {e}"))
}

/// Forget installers other than `keep` (older updates, broken downloads).
fn prune(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let ours = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("effectcraft-") && (n.ends_with(".msi") || n.ends_with(".part")));
        if ours && p != keep {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// One check, start to finish (on a worker thread).
fn check(dir: &Path, install: Install) -> std::result::Result<State, String> {
    let url = manifest_url();
    let manifest = fetch(&url)?;
    let sig = fetch(&format!("{url}.sig"))?;
    let m = verify_manifest(&manifest, &String::from_utf8_lossy(&sig), PUBLIC_KEY)?;
    let current = env!("CARGO_PKG_VERSION");
    match decide(&m, current, install)? {
        Found::UpToDate => Ok(State::UpToDate { latest: m.version }),
        Found::Tell => {
            let url = if m.page.starts_with("https://") { m.page } else { links::RELEASES.to_string() };
            Ok(State::Available { version: m.version, notes: m.notes, url })
        }
        Found::Install(a) => {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let dest = dir.join(installer_name(&m.version));
            prune(dir, &dest);
            if !verified(&dest, &a)? {
                download(&a, &dest)?;
            }
            Ok(State::Ready { version: m.version, notes: m.notes, path: dest.display().to_string() })
        }
    }
}

/// Start the installer once this process has exited, then start the app again (Windows).
#[cfg(target_os = "windows")]
fn launch_installer(msi: &Path) -> std::result::Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = msi.parent().ok_or("no folder")?;
    let script = dir.join("install-update.ps1");
    // Exit codes 0 and 3010 (reboot needed) mean it installed; anything else (1602: the user
    // declined) leaves the installer for the next try. Either way the app comes back.
    let body = r#"param([int]$AppPid, [string]$Msi, [string]$Exe, [string]$Log)
Wait-Process -Id $AppPid -Timeout 300 -ErrorAction SilentlyContinue
$p = Start-Process -FilePath "msiexec.exe" -ArgumentList "/i `"$Msi`" /passive /norestart /l*v `"$Log`"" -Wait -PassThru
if ($p.ExitCode -eq 0 -or $p.ExitCode -eq 3010) { Remove-Item -LiteralPath $Msi -ErrorAction SilentlyContinue }
Start-Process -FilePath $Exe
"#;
    std::fs::write(&script, body).map_err(|e| format!("cannot write {}: {e}", script.display()))?;
    let mut c = std::process::Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
        .arg(&script)
        .arg("-AppPid")
        .arg(std::process::id().to_string())
        .arg("-Msi")
        .arg(msi)
        .arg("-Exe")
        .arg(&exe)
        .arg("-Log")
        .arg(dir.join("install-update.log"));
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000 | 0x0000_0008); // CREATE_NO_WINDOW | DETACHED_PROCESS
    c.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "windows"))]
fn launch_installer(_msi: &Path) -> std::result::Result<(), String> {
    Err("installing updates is only available on Windows".into())
}

fn set(shared: &Arc<Mutex<Shared>>, f: impl FnOnce(&mut Shared)) {
    f(&mut shared.lock().unwrap_or_else(PoisonError::into_inner));
}

impl Session {
    /// Start a check (`quiet`: no toast unless there is something new; `wait`: before returning).
    pub fn check_for_updates(&mut self, quiet: bool, wait: bool) -> Result<Value> {
        if cfg!(target_arch = "wasm32") {
            return Err(bad("app.update.check", "the web app is always the latest version"));
        }
        if self.update.busy() {
            return Ok(json!({"started": false, "status": self.update.state()}));
        }
        let shared = self.update.shared.clone();
        let dir = updates_dir(self.config.as_ref().and_then(|c| c.dir()));
        let install = install_kind();
        set(&shared, |s| s.state = State::Checking);
        let run = move || {
            let r = check(&dir, install);
            set(&shared, |s| {
                s.done = match &r {
                    Ok(State::UpToDate { latest }) if !quiet => Some((format!("EffectCraft is up to date ({latest} is the newest version)"), false)),
                    Ok(State::Ready { version, .. }) => Some((format!("EffectCraft {version} is ready to install"), false)),
                    Ok(State::Available { version, .. }) => Some((format!("EffectCraft {version} is available"), false)),
                    Err(e) if !quiet => Some((format!("Check for Updates: {e}"), true)),
                    _ => None,
                };
                s.state = r.unwrap_or_else(|error| State::Failed { error });
            });
        };
        if wait {
            run();
        } else if let Err(e) = std::thread::Builder::new().name("ec-update".into()).spawn(run) {
            set(&self.update.shared, |s| s.state = State::Failed { error: format!("cannot start: {e}") });
        }
        if let Some(m) = self.update.shared.lock().unwrap_or_else(PoisonError::into_inner).done.take().filter(|_| wait) {
            self.events.push(Event::Toast { message: m.0, error: m.1 });
        }
        Ok(json!({"started": true, "status": self.update.state()}))
    }

    /// Frontends call this every frame: a finished check's toast, and the daily automatic check.
    pub fn poll_updates(&mut self) {
        let done = self.update.shared.lock().unwrap_or_else(PoisonError::into_inner).done.take();
        if let Some((message, error)) = done {
            self.events.push(Event::Toast { message, error });
        }
        if !self.update.auto || !self.prefs.startup.check_for_updates {
            return;
        }
        let due = self.update.last_auto.is_none_or(|t| t.elapsed().as_secs() >= CHECK_EVERY_SECS);
        if due && !self.update.busy() {
            self.update.last_auto = Some(web_time::Instant::now());
            // A failed automatic check stays quiet: no network is not an error worth a toast.
            let _ = self.check_for_updates(true, false);
        }
    }
}

fn status(s: &mut Session) -> Result<Value> {
    let install = install_kind();
    Ok(json!({
        "current": env!("CARGO_PKG_VERSION"),
        "status": s.update.state(),
        "install": match install { Install::Msi => "msi", Install::Portable => "portable", Install::Manual => "manual" },
        "platform": platform_key(install),
        "manifest": manifest_url(),
        "automatic": s.prefs.startup.check_for_updates,
        "dismissed": s.update.dismissed,
    }))
}

/// Restart to Update: quit (asking to save a modified project), install, start again.
fn install(s: &mut Session) -> Result<Value> {
    let State::Ready { version, .. } = s.update.state() else {
        return Err(bad("app.update.install", "no update is ready to install (app.update.check finds one)"));
    };
    s.update.install_on_exit = true;
    s.events.push(Event::Frontend { command: "app.quit".into(), params: json!({}) });
    Ok(json!({"installing": version}))
}

/// Later: hide the offer until the next launch.
fn dismiss(s: &mut Session) -> Result<Value> {
    s.update.dismissed = match s.update.state() {
        State::Ready { version, .. } | State::Available { version, .. } => Some(version),
        _ => None,
    };
    Ok(json!({"dismissed": s.update.dismissed}))
}

/// Open the release page of an available update (portable, macOS, Linux).
fn open_page(s: &mut Session) -> Result<Value> {
    let url = match s.update.state() {
        State::Available { url, .. } => url,
        _ => links::RELEASES.to_string(),
    };
    s.events.push(Event::OpenUrl(url.clone()));
    Ok(json!({"url": url}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "app.update.check",
            "Check for Updates...",
            // Help ▸ Check for Updates… on Windows and Linux only (macOS updates by hand).
            [],
            None,
            "{quiet?, wait?} — finds, downloads and verifies a newer release (Windows installs it with app.update.install)",
            always,
            |s, p| s.check_for_updates(b_p(p, "quiet").unwrap_or(false), b_p(p, "wait").unwrap_or(false))
        ),
        query!("app.update.status", "Update Status", "{}", |s, _| status(s)),
        cmd!(
            "app.update.install",
            "Restart to Update",
            [],
            None,
            "{} — quits (asking to save), installs the downloaded update, starts again",
            always,
            |s, _| install(s)
        ),
        cmd!("app.update.dismiss", "Hide Update Until Next Launch", [], None, "{}", always, |s, _| dismiss(s)),
        cmd!("app.update.openPage", "Open the Update's Release Page", [], None, "{}", always, |s, _| open_page(s)),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn manifest(version: &str) -> Manifest {
        let mut platforms = BTreeMap::new();
        platforms.insert(platform_key(Install::Msi), Asset { url: "https://example.com/e.msi".into(), size: 10, sha256: "ab".repeat(32) });
        Manifest { version: version.into(), page: "https://example.com/r".into(), notes: "n".into(), date: String::new(), platforms }
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("0.9.142"), Some((0, 9, 142)));
        assert_eq!(parse_version("v1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("one"), None);
        assert!(is_newer("0.9.110", "0.9.2"));
        assert!(is_newer("0.10.0", "0.9.999"));
        assert!(!is_newer("0.9.2", "0.9.2"));
        assert!(!is_newer("0.9.1", "0.9.2"));
        assert!(!is_newer("garbage", "0.9.2"));
    }

    #[test]
    fn signed_manifests_verify_and_tampered_ones_dont() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = hex(key.verifying_key().as_bytes());
        let bytes = serde_json::to_vec(&manifest("0.9.200")).unwrap();
        let sig = hex(&key.sign(&bytes).to_bytes());
        assert_eq!(verify_manifest(&bytes, &sig, &public).unwrap().version, "0.9.200");
        assert_eq!(verify_manifest(&bytes, &format!("{sig}\n"), &public).unwrap().version, "0.9.200");
        let mut tampered = bytes.clone();
        tampered[5] ^= 1;
        assert!(verify_manifest(&tampered, &sig, &public).is_err());
        let other = hex(SigningKey::from_bytes(&[8u8; 32]).verifying_key().as_bytes());
        assert!(verify_manifest(&bytes, &sig, &other).is_err());
        assert!(verify_manifest(&bytes, "zz", &public).is_err());
        assert!(verify_manifest(&bytes, &sig, "not a key").is_err());
    }

    #[test]
    fn the_shipped_key_is_valid() {
        let key = hex_bytes::<32>(PUBLIC_KEY).unwrap();
        assert!(ed25519_dalek::VerifyingKey::from_bytes(&key).is_ok());
    }

    #[test]
    fn decide_installs_only_newer_valid_entries() {
        let m = manifest("0.9.200");
        assert!(matches!(decide(&m, "0.9.200", Install::Msi), Ok(Found::UpToDate)));
        assert!(matches!(decide(&m, "0.9.2", Install::Msi), Ok(Found::Install(_))));
        assert!(matches!(decide(&m, "0.9.2", Install::Portable), Ok(Found::Tell)));
        assert!(matches!(decide(&m, "0.9.2", Install::Manual), Ok(Found::Tell)));
        let mut bad = m.clone();
        bad.platforms.values_mut().for_each(|a| a.url = "http://example.com/e.msi".into());
        assert!(decide(&bad, "0.9.2", Install::Msi).is_err());
        let mut huge = m.clone();
        huge.platforms.values_mut().for_each(|a| a.size = u64::MAX);
        assert!(decide(&huge, "0.9.2", Install::Msi).is_err());
        let mut none = m;
        none.platforms.clear();
        assert!(decide(&none, "0.9.2", Install::Msi).is_err());
    }

    #[test]
    fn installer_names_stay_in_the_folder() {
        assert_eq!(installer_name("0.9.142"), "effectcraft-0.9.142-update.msi");
        assert_eq!(installer_name("../../evil"), "effectcraft-....evil-update.msi");
    }

    #[test]
    fn downloads_are_checked_against_the_manifest() {
        let dir = std::env::temp_dir().join(format!("ec-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("effectcraft-1.0.0-update.msi");
        std::fs::write(&f, b"installer!").unwrap();
        let good = Asset { url: String::new(), size: 10, sha256: effectcraft_segment::sha256::hex(b"installer!") };
        assert!(verified(&f, &good).unwrap());
        assert!(!verified(&f, &Asset { size: 11, ..good.clone() }).unwrap());
        assert!(!verified(&f, &Asset { sha256: "00".repeat(32), ..good.clone() }).unwrap());
        assert!(!verified(&dir.join("missing.msi"), &good).unwrap());
        let old = dir.join("effectcraft-0.9.0-update.msi");
        std::fs::write(&old, b"x").unwrap();
        prune(&dir, &f);
        assert!(f.exists() && !old.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_needs_a_ready_update_and_later_hides_it() {
        let mut s = Session::new();
        assert!(s.execute("app.update.install", json!({})).is_err());
        set(&s.update.shared, |sh| sh.state = State::Ready { version: "9.9.9".into(), notes: String::new(), path: "x.msi".into() });
        assert!(s.update.offer().is_some());
        s.execute("app.update.dismiss", json!({})).unwrap();
        assert!(s.update.offer().is_none());
        s.events.clear();
        s.execute("app.update.install", json!({})).unwrap();
        assert!(s.update.install_on_exit);
        assert!(s.events.iter().any(|e| matches!(e, Event::Frontend { command, .. } if command == "app.quit")));
    }

    #[test]
    fn headless_sessions_never_check_by_themselves() {
        let mut s = Session::new();
        s.poll_updates();
        assert_eq!(s.update.state(), State::Idle);
    }
}
