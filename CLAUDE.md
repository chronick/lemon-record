# CLAUDE.md — LEMON record

Guidance for AI agents working in this repo.

## What this is

**LEMON record** — a standalone multi-track audio recorder, branded under
**Lemon Audio**. It does *only* recording: capture multichannel input to files.
No analysis, no sample library, no database. Analysis / sample-management tooling
lives in `~/git/sample-kit`; do not add it here. Keep this app a pure recorder.

Built on the agent-as-UI philosophy (`~/.claude/skills/agent-ui-creator`,
vault `context/principles/agent-as-ui.md`): a small standalone GUI for the one
realtime job, coupling to anything downstream only through files.

## Crates

| Crate | Role |
|-------|------|
| `recorder` | Capture core **library** — no GUI. `config` (file-backed), `arm` (on/off/auto master gate), `session` (auto-segmenting takes + per-take WAVs + manifest), `capture` (the cpal stream), `metering`/`visualization` DSP, `naming`. |
| `lemon-record` | The egui app → binary `lemon-record`. Drives the `recorder` library directly. |

## Model (important)

- **Master-gated takes.** The master (summed mix of non-muted tracks) runs the
  arm state machine. Each gate-open starts a take; gate-close (after the hold)
  finalizes it and re-arms. `On` = one continuous take; `Off` = idle. The whole
  per-take state (clock, files) resets between takes while staying armed.
- **Tracks are a mixer.** `muted` excludes a track from the master *and* from
  recording; `volume` scales its master contribution (stems stay raw/unity).
- **Master is primary.** It's always written; per-track stems are written only
  when more than one track is active.
- **Config is the canonical interface** — `~/.music-hub-data/sample-recorder-config.json`.
  Every surface reads + writes the same file. New settings get `#[serde(default)]`
  so old files keep loading; the GUI round-trips them.
- **Keep the pure core pure.** `arm` and `session` have no audio-hardware
  dependency on purpose — that's why they're exhaustively unit/e2e tested. New
  routing/gating logic goes there (testable), not in `capture` (the thin shell).

## Build & test

```bash
cargo build --workspace
cargo test  --workspace      # recorder unit (config/arm/session/naming/metering/viz)
                             #   + tests/session_e2e.rs
cargo run   -p lemon-record
```

`session_e2e.rs` drives a full multi-take session with synthetic audio and reads
the WAVs + manifest back off disk — run it after any change to
`session`/`capture`/`config`. It opportunistically validates with `ffprobe`.

The realtime audio callback (`capture::process_block`) must not block —
visualization locks are `try_lock`; the writer send is a regular lock (dropping
audio is unacceptable, dropping a meter frame is invisible).

## Releases

Production pipeline (CI artifacts, semver, **auto-update**, signing) is tracked
in beads (vault repo) — every future feature/fix ships through it. Don't hand-cut
releases; follow RELEASING.md once it exists.

## Task tracking

This workspace uses **beads** (`br`) in `~/git/vault/.beads/`. Use the label
`repo:lemon-record`. File substantial work before starting; close with a reason.
