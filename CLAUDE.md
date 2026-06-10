# Claude Instructions — IRIS

IRIS is an SGI Indy (MIPS R4400) emulator written in Rust. It boots IRIX 6.5
and 5.3 to a usable system (shell, networking, X11). It is **not** cycle-accurate
— IRIX doesn't need it and accuracy would only make it slower.

## Read these first

- `HACKING.md` — architecture: data path/endianness, concurrency model, the
  MC bus/device/port abstraction. **Read before touching device or CPU code.**
- `HELP.md` — running it: serial ports, monitor console, NVRAM/MAC setup, disk
  image prep.
- `README.md` — overview, feature flags, current status.
- `docs/` — per-device notes (hal2, rex3, irix-install, …).
- `rules/` — accumulated, hard-won findings about emulator behaviour
  (`jit/`, `snapshot/`, `irix/`, `testing/`). Check here before re-deriving a
  gotcha; when you confirm a non-obvious fix, write it up here as a short
  markdown note so the next session doesn't relearn it.

## Build & run

```
cargo run --release                                       # interpreter
cargo run --release --features lightning,rex-jit,tlbvmap  # recommended for speed
IRIS_JIT=1 cargo run --release --features jit             # enable MIPS JIT
```

Binaries: `iris` (the emulator), `iris-ci` (CI/automation socket client),
`coffdump`, `chd_extract`. Feature flags and `IRIS_JIT_*` env vars are
documented in `README.md`.

## Hard invariants (from HACKING.md)

- **Endianness lives only at "The Edge."** Host `u32`/`u64` are bit-containers;
  byte-swapping happens at PROM/disk I/O via `swap_on_load`, never in CPU/bus/MC
  logic. **Do not suggest `.to_be()` / `.to_le()` for memory or register code.**
- **Concurrency is per-device.** CPU, REX3, SCSI, and ethernet run on their own
  threads and lock their own state. Deadlocks live in callbacks *up* to a parent
  device (e.g. SCSI → HPC3) — be careful there.

## Automation & CI

- `iris-ci` is the canonical socket interface for driving a running emulator
  (snapshots, scripted input, headless runs). Prefer it over ad-hoc serial
  poking. See `rules/snapshot/` and `manual_test_runbook.md`.
- Install IRIX only from original media (see `docs/irix-install.md`). Never use a
  pre-built MAME CHD as a shortcut.
- After changing PROM env (`setenv`/`unsetenv`) or NVRAM, run `rtc save` from the
  monitor console before halting, or the change is lost.

## Fork & branch policy (danifunker fork)

This is a fork of `techomancer/iris`. **Two kinds of change live in two
different places — never mix them.** Upstream-appropriate work goes back to
techomancer in a PR; fork-specific build/packaging/distribution work stays on
the build branch and is never sent upstream.

### Branches

- **`main`** — a mirror of `techomancer/iris` plus *only* the three workflow
  files (`.github/workflows/{release,sync-upstream,appstore}.yml`), which must
  live on the default branch for Actions to schedule/display them. Kept current
  by `sync-upstream.yml` (nightly rebase onto `upstream/main`).
- **`build-pipeline-danifunker`** — the fork working branch. **All fork-specific
  / pipeline / packaging / distribution work lives here and nowhere else.** This
  is the branch the Release and App Store pipelines build.
- **`upstream-improvements`** — the branch used to PR general improvements back
  to `techomancer/iris`. **Always branched from `upstream/main`, never from
  `danifunker/main`** (see the gotcha below).

`upstream` remote = `https://github.com/techomancer/iris.git`
(run `git remote add upstream https://github.com/techomancer/iris.git` if missing).

The split is **path-based** (policy decision, 2026-06-10). The emulator and all
its build inputs live upstream so the nightly rebase stays clean and we can pull
upstream fixes; only this fork's CI orchestration, backups, and fork docs stay on
the branch.

### Goes UPSTREAM (mirror every change to `upstream-improvements`)

These paths should be kept **identical** between `build-pipeline-danifunker` and
`upstream-improvements`. Any edit here is mirrored to the PR branch:

- `src/**` — the emulator (iris lib + binaries).
- `iris-gui/**` — the GUI, **including** its `Cargo.toml` packaging metadata
  (`[package.metadata.deb|generate-rpm]`, `description`/`license`),
  `iris-gui.desktop`, `build.rs`, and the `appstore` cargo feature + its gated
  source (`macos_sandbox.rs`, `cfg!(feature = "appstore")` gating, macOS-target
  `objc2*` deps). The `appstore` feature is a no-op off the App Store build, but
  it lives in shared source so upstream gets it too.
- `Cargo.toml` (workspace root), `profile.sh`.
- `installer/**` (macOS entitlements, Inno Setup `.iss`).
- `scripts/build-macos.sh`.

### STAYS on `build-pipeline-danifunker` (never upstream)

Only this fork's CI orchestration + backups + fork docs:

- `.github/workflows/**` (`release.yml`, `sync-upstream.yml`, `appstore.yml`,
  and `rust.yml` tweaks).
- `backup/**`.
- `docs/**` (`appstore-build.md`, `appstore-listing.md`, `handoff-pipeline.md`).
- `CLAUDE.md` (this policy) and `LICENSE-GPL3.txt` (vendored for the pipeline).

Because the split is purely path-based there are no "mixed" files: a path is
either fully upstream-bound or fully fork-only.

### The gotcha: a PR diff is computed against the *target*, not your fork

`danifunker/main` carries the three workflow files on top of an old
`techomancer/main` commit, so a PR branched from `danifunker/main` shows those
1300+ lines of workflows in its diff against `techomancer/main`. **Always build
the PR branch on `upstream/main` and copy in only the files you want:**

```bash
git fetch upstream main
git switch -c <pr-branch> upstream/main
git checkout build-pipeline-danifunker -- <only the files for upstream>
git commit && git push -u origin <pr-branch>
git diff --name-only upstream/main <pr-branch>   # confirm scope before PR
```

Before opening or refreshing a PR, run
`git diff --name-only upstream/main upstream-improvements` and confirm the list
contains **only** upstream-appropriate files — no workflows, no `installer/`, no
`scripts/build-macos.sh`, no packaging metadata.
