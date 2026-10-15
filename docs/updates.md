# Releases and updates (fork)

Every push to `main` builds and publishes a release, and installed Windows builds update
themselves from it. macOS builds are published too, but update by hand.

## The pipeline

[`.github/workflows/fork-release.yml`](../.github/workflows/fork-release.yml) runs on every push
to `main` (pushes that only touch `docs/`, Markdown or `contributors/` are skipped) and by hand
from the Actions tab.

1. **version**: `MAJOR.MINOR` from `[workspace.package] version` in `Cargo.toml`, and
   `PATCH = run number + 100`, for example `0.9.142`. Versions only ever go up, which the updater
   relies on, and stay within the MSI's limit (`PATCH` ≤ 65535).
2. **macos** (`macos-15`): `packaging/macos/package.sh --arch universal`, giving
   `effectcraft-<v>-macos-universal.dmg` and the CLI zip. Ad-hoc signed unless the `APPLE_*`
   secrets are set.
3. **windows** (`windows-latest`): `packaging/windows/package.ps1 -Arch x64`, giving
   `effectcraft-<v>-windows-x64.msi` and the portable zip. Unsigned unless `WINDOWS_CERTIFICATE`
   is set.
4. **publish**: `cargo xtask update-manifest` writes `latest.json` (URL, size and SHA-256 of
   each file) and signs it into `latest.json.sig` with `UPDATE_SIGNING_KEY`, then
   `gh release create v<v> … --latest` publishes everything, with the commit subjects since the
   previous release as notes.

A newer push cancels a build that is still running, so only the newest `main` is released. The
publish job only runs when both builds succeed.

## How Windows updates

At launch, and once a day while it runs (Settings ▸ Startup & Repair ▸ Automatically Check for
Updates; Help ▸ Check for Updates… at any time), the app:

1. fetches `releases/latest/download/latest.json` and `.sig` from this repository with the
   system's `curl`, HTTPS only;
2. checks the Ed25519 signature against `update::PUBLIC_KEY`, built into the app. An unsigned or
   tampered manifest is refused;
3. if the version is newer, downloads the MSI to `%LOCALAPPDATA%\EffectCraft\Updates` and checks
   its size and SHA-256 against the manifest;
4. shows **EffectCraft <v> is ready to install** (bottom right) with **Restart to Update** and
   **Later**. Later hides it until the next launch;
5. on Restart to Update, quits normally (a modified project asks to be saved first). A hidden
   PowerShell helper waits for the app to exit, runs `msiexec /i … /passive` (Windows asks for
   administrator permission, because the MSI installs per machine) and starts EffectCraft again.
   The MSI's `MajorUpgrade` replaces the old version in its folder. The log goes to
   `%LOCALAPPDATA%\EffectCraft\Updates\install-update.log`.

The portable build, macOS and Linux only show a **Download** link to the release. Agents
(`--control`), the CLI and tests never check on their own.

### Why no code-signing certificate is needed

- What proves that an update is ours is the manifest signature, not Authenticode. Only the
  release workflow holds the private key.
- Files downloaded with `curl` carry no Mark-of-the-Web, so SmartScreen does not stop the update
  install.
- The first install, downloaded in a browser, does get SmartScreen's "Windows protected your PC"
  warning: More info ▸ Run anyway. A certificate (`WINDOWS_CERTIFICATE` /
  `WINDOWS_CERTIFICATE_PASSWORD` secrets) removes it; nothing else changes.

## One-time setup

1. **Signing key.** `cargo xtask update-keygen ~/.effectcraft-update-signing-key` writes a new
   secret key, which must stay private, and prints the public key. The public key goes into
   `PUBLIC_KEY` in `crates/engine/src/update.rs`; the secret goes into the repository's
   **Settings ▸ Secrets and variables ▸ Actions** as `UPDATE_SIGNING_KEY` (the file's 64 hex
   digits).
   Losing the secret means installed apps can no longer update: they only trust its key. Keep a
   backup.
2. **Workflow permissions.** The publish job asks for `contents: write`; nothing else is needed.
3. **First install.** Builds made before the updater existed can't update themselves: install a
   release from this pipeline by hand once. Every later release then arrives automatically.

## Operating it

- **Pull a bad release:** mark it as a pre-release (or delete it) on GitHub. `latest` then points
  at the previous release and no more installs pick the bad one up. Apps never downgrade, so ship
  the fix as a new push.
- **Test an update locally:** serve a folder with a `latest.json` / `latest.json.sig` made by
  `cargo xtask update-manifest` and start the app with
  `EFFECTCRAFT_UPDATE_URL=http://127.0.0.1:8000/latest.json`. The override also allows `http://`.
- **Control channel / MCP:** `app.update.check {quiet?, wait?}`, `app.update.status`,
  `app.update.install`, `app.update.dismiss`, `app.update.openPage`.
- **Rotate the key:** ship a release containing the new `PUBLIC_KEY` while it is still signed
  with the old secret, then switch the `UPDATE_SIGNING_KEY` secret.
