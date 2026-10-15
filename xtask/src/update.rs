//! Update manifests (docs/updates.md, `crates/engine/src/update.rs`).
//!
//! - `update-keygen <secret-file>`: a new Ed25519 key pair. The secret (hex) goes into the file,
//!   which must not exist yet, and then into the `UPDATE_SIGNING_KEY` secret of the release
//!   workflow; the public key printed goes into `update::PUBLIC_KEY`.
//! - `update-manifest --version V --dir DIR --repo OWNER/NAME [--notes FILE]`: write
//!   `DIR/latest.json` listing the release files in DIR (URL, size, SHA-256 per platform) and
//!   `DIR/latest.json.sig`, its signature with the key in `$UPDATE_SIGNING_KEY`.

use std::collections::BTreeMap;
use std::path::Path;

use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex32(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    if s.len() != 64 {
        return Err("UPDATE_SIGNING_KEY must be 64 hex digits (cargo xtask update-keygen)".into());
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| "UPDATE_SIGNING_KEY is not hex".to_string())?;
    }
    Ok(out)
}

fn keygen(out: &Path) -> Result<(), String> {
    if out.exists() {
        return Err(format!("{} exists; refusing to overwrite a signing key", out.display()));
    }
    let mut seed = [0u8; 32];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom").map_err(|e| format!("/dev/urandom: {e}"))?, &mut seed).map_err(|e| e.to_string())?;
    let key = SigningKey::from_bytes(&seed);
    std::fs::write(out, format!("{}\n", hex(&seed))).map_err(|e| format!("write {}: {e}", out.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    println!("secret key written to {} (keep it private; set it as the UPDATE_SIGNING_KEY secret)", out.display());
    println!("public key (crates/engine/src/update.rs PUBLIC_KEY): {}", hex(key.verifying_key().as_bytes()));
    Ok(())
}

/// The manifest platform a release file is for, by its name (`effectcraft-<v>-<platform>.<ext>`).
fn platform_of(name: &str, version: &str) -> Option<String> {
    let rest = name.strip_prefix(&format!("effectcraft-{version}-"))?;
    if let Some(p) = rest.strip_suffix("-portable.zip") {
        return Some(format!("{p}-portable"));
    }
    rest.strip_suffix(".msi").or_else(|| rest.strip_suffix(".dmg")).map(str::to_string)
}

fn manifest(args: &[String]) -> Result<(), String> {
    let mut opt: BTreeMap<&str, &str> = BTreeMap::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = it.next().ok_or_else(|| format!("{a} needs a value"))?;
        opt.insert(a.as_str(), v.as_str());
    }
    let need = |k: &str| opt.get(k).copied().ok_or_else(|| format!("missing {k}"));
    let (version, dir, repo) = (need("--version")?, Path::new(need("--dir")?), need("--repo")?);
    let notes = match opt.get("--notes") {
        Some(f) => std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?.trim().to_string(),
        None => String::new(),
    };
    let key = SigningKey::from_bytes(&unhex32(&std::env::var("UPDATE_SIGNING_KEY").map_err(|_| "UPDATE_SIGNING_KEY is not set")?)?);

    let mut platforms = serde_json::Map::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(platform) = platform_of(&name, version) else { continue };
        let bytes = std::fs::read(e.path()).map_err(|e| format!("{name}: {e}"))?;
        platforms.insert(
            platform,
            json!({
                "url": format!("https://github.com/{repo}/releases/download/v{version}/{name}"),
                "size": bytes.len(),
                "sha256": hex(&Sha256::digest(&bytes)),
            }),
        );
    }
    if platforms.is_empty() {
        return Err(format!("no effectcraft-{version}-* release files in {}", dir.display()));
    }
    let m: Value = json!({
        "version": version,
        "page": format!("https://github.com/{repo}/releases/tag/v{version}"),
        "date": std::env::var("EFFECTCRAFT_BUILD_DATE").unwrap_or_default(),
        "notes": notes,
        "platforms": platforms,
    });
    let bytes = serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?;
    let sig = key.sign(&bytes);
    std::fs::write(dir.join("latest.json"), &bytes).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("latest.json.sig"), hex(&sig.to_bytes())).map_err(|e| e.to_string())?;
    println!("{}", String::from_utf8_lossy(&bytes));
    println!("signed with {}", hex(key.verifying_key().as_bytes()));
    Ok(())
}

pub fn run(task: &str, args: &[String]) -> Result<(), String> {
    match (task, args) {
        ("update-keygen", [out]) => keygen(Path::new(out)),
        ("update-manifest", rest) => manifest(rest),
        _ => Err("usage: cargo xtask update-keygen <secret-file> | update-manifest --version V --dir DIR --repo OWNER/NAME [--notes FILE]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_files_map_to_platforms() {
        assert_eq!(platform_of("effectcraft-0.9.142-windows-x64.msi", "0.9.142").as_deref(), Some("windows-x64"));
        assert_eq!(platform_of("effectcraft-0.9.142-windows-x64-portable.zip", "0.9.142").as_deref(), Some("windows-x64-portable"));
        assert_eq!(platform_of("effectcraft-0.9.142-macos-universal.dmg", "0.9.142").as_deref(), Some("macos-universal"));
        assert_eq!(platform_of("effectcraft-cli-0.9.142-macos-universal.zip", "0.9.142"), None);
        assert_eq!(platform_of("effectcraft-0.9.141-windows-x64.msi", "0.9.142"), None);
    }
}
