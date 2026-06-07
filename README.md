# LEMON record

A small, standalone **multi-track audio recorder** — part of **Lemon Audio**.

It does one thing: capture multichannel input to files, cleanly. Arm once and
every phrase you play becomes its own saved take. No library, no analysis, no
database — just a recorder that writes WAVs you can drag anywhere.

Aesthetic: Ableton meets a terminal — flat dark panels, a monospace face, a
lemon-yellow brand accent, channel-strip meters.

## What it does

- **Multi-track.** Opens a multichannel input at its native channel count (a
  Zoom L6Max is up to 12 mono / 6 stereo). The **master** is the summed mix; each
  track is a slice of the input channels.
- **Master-gated auto-take recording.** Arm the master (`Auto`) and it records
  only while the signal is above a threshold, finalizing a **take** after a
  configurable hold and re-arming for the next — set and forget. `On` records one
  continuous take; `Off` is idle.
- **Basic mixer.** Per-track **mute** (excludes from master + recording) and
  **volume** (scales the master mix; stems stay raw).
- **File-backed config, bidirectional.** Everything lives in
  `~/.music-hub-data/sample-recorder-config.json` — editable by hand, by the GUI,
  or by an agent. One source of truth.
- **Self-describing output.** Unique, human-readable filenames so any WAV can be
  dragged anywhere without context.

## Output layout

One folder per armed session, one set of files per take:

```
~/.music-hub-data/recordings/<name>-<datetime>/
├── <name>-<id>-t01-master.wav     # take 1 mix (always)
├── <name>-<id>-t01-<track>.wav    # take 1 stems (only when >1 track)
├── <name>-<id>-t02-master.wav     # take 2 …
└── session.json                   # manifest of every take (rewritten as they close)
```

`<name>` is a heroku-style auto name (e.g. `amber-cascade`) unless you pin one.

## Build & run

```bash
cargo run -p lemon-record       # launch the recorder window
cargo build --workspace
cargo test  --workspace         # capture core unit tests + session e2e
```

## Layout

```
lemon-record/
├── crates/
│   ├── recorder/        # capture core library (config, arm, session, capture, dsp)
│   └── lemon-record/    # the egui app → binary `lemon-record`
└── Cargo.toml           # workspace
```

## Install

Download the latest `.dmg` from [Releases](https://github.com/chronick/lemon-record/releases),
open it, and drag **LEMON record** to Applications.

Builds are currently ad-hoc signed (not yet Apple-notarized), so on first launch
clear the Gatekeeper quarantine flag once:

```bash
xattr -dr com.apple.quarantine "/Applications/LEMON record.app"
# or: right-click the app → Open → Open
```

The app self-updates from there: **Settings → SOFTWARE UPDATE → Check for updates**.

## Releases

Tag-driven: push `vX.Y.Z` and CI builds the `.app` + `.dmg`, signs the updater
artifact, and drafts a GitHub Release with an auto-update manifest. Full ritual,
versioning, and the signing decision: **[RELEASING.md](RELEASING.md)**.

## Lineage

The capture core was originally built inside `sample-curator` (a Tauri sample
manager, now frozen) and the `sample-kit` pipeline; the recorder was split out
here as its own focused app. Analysis / sample-management tooling lives in
`sample-kit`, not here — LEMON record stays a pure recorder.

Built on the agent-as-UI philosophy: a deterministic substrate an agent can
drive, with a small standalone GUI for the one realtime job (capture with meters).
