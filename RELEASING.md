# Releasing LEMON record

Tag-driven release engineering: push a `vX.Y.Z` tag and CI builds the macOS
`.app` + `.dmg`, signs the updater artifact, generates `latest.json`, and
attaches everything to a **draft** GitHub Release. You review and publish; the
in-app auto-updater then sees the new version.

- Packaging: [`cargo-packager`](https://docs.crabnebula.dev/packager/) 0.11.8 (`.app` + `.dmg`).
- Auto-update: [`cargo-packager-updater`](https://docs.rs/cargo-packager-updater) 0.2.3 (minisign-verified, swaps the running `.app`).
- Config lives in [`crates/lemon-record/Cargo.toml`](crates/lemon-record/Cargo.toml) `[package.metadata.packager]`.
- CI: [`.github/workflows/release.yml`](.github/workflows/release.yml).

## Versioning (single source)

The version is defined **once**, in the workspace root [`Cargo.toml`](Cargo.toml)
under `[workspace.package] version`. Both crates inherit it via
`version.workspace = true`, the binary reports it through `env!("CARGO_PKG_VERSION")`,
and cargo-packager reads it via cargo-metadata. Never set a version anywhere else.

## One-time setup

### 1. Updater signing key (required — auto-update needs it)

A minisign keypair signs every update so the installed app only accepts builds
you signed. Generate once:

```bash
cargo packager signer generate --path ~/.lemon-record/updater.key
```

This writes the private key (`updater.key`) and public key (`updater.key.pub`).
The **public** key is committed at `crates/lemon-record/updater.pub` (embedded in
the binary). The **private** key never leaves your machine / Secrets.

Add two GitHub repo secrets (Settings → Secrets and variables → Actions):

| Secret | Value |
| ------ | ----- |
| `CARGO_PACKAGER_SIGN_PRIVATE_KEY` | contents of `~/.lemon-record/updater.key` |
| `CARGO_PACKAGER_SIGN_PRIVATE_KEY_PASSWORD` | the password you set |

> If you rotate this key, you must also replace `crates/lemon-record/updater.pub`
> and ship a build with the new pubkey **before** the next signed release, or
> installed apps will reject the update.

### 2. Apple Developer ID signing + notarization (optional — see decision below)

Add these only when you're ready to ship a notarized, workaround-free build:

| Secret | Value |
| ------ | ----- |
| `APPLE_CERTIFICATE` | base64 of your Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` export password |
| `APPLE_ID` | your Apple ID email |
| `APPLE_PASSWORD` | an app-specific password |
| `APPLE_TEAM_ID` | your 10-char Team ID |

cargo-packager signs + notarizes automatically when these are present. When
absent, the build is ad-hoc signed (see workaround below).

## Cutting a release

```bash
# 1. Bump the single source of truth.
#    edit Cargo.toml -> [workspace.package] version = "0.1.1"
# 2. Commit + tag (tag must match the version, with a leading v).
git commit -am "Release v0.1.1"
git tag v0.1.1
git push origin main --tags
```

CI runs `release.yml`, then:

```
3. Review the DRAFT release on GitHub (artifacts + latest.json attached).
4. Publish it. The updater reads the latest *published* (non-draft) release,
   so nothing self-updates until you publish.
```

Released assets per tag:

| Asset | Purpose |
| ----- | ------- |
| `LEMON-record_<ver>_<arch>.dmg` | human download / install |
| `LEMON-record_<ver>_<arch>.app.tar.gz` (+ `.sig`) | updater payload (signed) |
| `latest.json` | updater manifest (served at `releases/latest/download/latest.json`) |
| `SHA256SUMS` | checksums |

## How auto-update works

The app polls `https://github.com/chronick/lemon-record/releases/latest/download/latest.json`
(Settings → SOFTWARE UPDATE → Check for updates). If the manifest's `version` is
newer, it offers Download & install; the updater verifies the minisign signature,
swaps the running `.app` in place, and you click Relaunch. There is no automatic
restart on macOS — the app re-opens the new bundle and exits.

### Demonstrating it end-to-end

The acceptance gate is a live self-update. To prove it:

1. Ship **v0.1.0**: tag, let CI build, **publish** the draft. Install the `.dmg`.
2. Bump to **v0.1.1**, tag, let CI build, **publish**.
3. In the installed v0.1.0, Check for updates → it offers v0.1.1 → install → relaunch → version reads v0.1.1.

> "Check for updates" reporting `Could not fetch a valid release JSON` simply
> means there is no published release yet (the `latest.json` URL 404s). That is
> the expected state before step 1 — the updater is working; it just has nothing
> to find.

## Signing / notarization decision

**Status: deferred, with a documented workaround.** v0.1.x ships **ad-hoc signed**
(no Apple Developer ID). The auto-updater's minisign signing is fully wired and
required — that is independent of Apple signing and is **not** deferred.

Rationale: Apple Developer ID requires a paid account + cert management, and the
auto-update acceptance gate does not depend on it. The CI workflow already wires
the `APPLE_*` secrets, so enabling notarization later is purely additive — add the
secrets and re-tag.

Until then, first launch needs the one-time Gatekeeper workaround:

```bash
# After dragging the app to /Applications:
xattr -dr com.apple.quarantine "/Applications/LEMON record.app"
# or: right-click the app -> Open -> Open (once).
```

This is also noted in the README for end users.
