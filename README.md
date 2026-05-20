Me and my homies Claude and Gemini present:


# IRIS — Irresponsible Rust IRIX Simulator

An SGI Indy emulator, vibed into existence with Rust and AI assistance.
Boots IRIX 6.5 and 5.3. Has networking. Has a framebuffer.

![IRIS running IRIX 6.5](screen.png)


## Q&A

**Q: What is it?**

**A:** An SGI Indy (MIPS R4400) emulator. Emulates enough hardware that IRIX
boots to a usable system: shell, networking, X11, the works.

**Q: But why?**

**A:** Wanted to see how far vibe coding could go, and to learn some Rust along the way.

**Q: You could have improved MAME.**

**A:** Didn't seem like fun.

**Q: So did you learn Rust?**

**A:** LOL, my brain hurts. Let's not get ahead of ourselves.

**Q: What LLMs did you use?**

**A:** Mostly Claude, some Gemini. They wrote a lot of the hard parts. (This was written by Claude, the humble AI assistant).

**Q: Can I contribute?**

**A:** Yes, bug reports and merge requests are welcome.

**Q: Regrets?**

**A:** Yes.


## Current status

- IRIX 6.5 boots to multiuser, networking works (ping, telnet, ftp)
- IRIX 5.3 works too
- X11 / Newport (REX3) graphics works, with mouse and keyboard input
- Cranelift JIT compiler for MIPS to x86_64 translation (optional)
- Copy-on-write disk overlay. Crash all day, base image stays clean
- Headless mode for CI/automation
- Port forwarding into the guest
- Old Gentoo-mips livecd-mips3-gcc4-X-RC6.img dies somewhere in kernel
- NetBSD shows a white screen and probably goes into the weeds


## Getting started

You need:
- `scsi1.raw` — raw hard disk image with IRIX 6.5.22 for Indy
  (for a quick start get the MAME IRIX image from https://mirror.rqsall.com/sgi-mame/ and convert to raw using `chdman extractraw`)
- `070-9101-011.bin` — Indy PROM image (optional; a default is embedded)

```
cargo run --release
```

Build variants:
```
cargo run --release --features lightning             # disable emulator breakpoints for a little bit more speed
cargo run --release --features jit                   # enable Cranelift MIPS JIT compiler
cargo run --release --features rex-jit               # enable REX3 graphics JIT compiler
cargo run --release --features tlbvmap               # enable 8k slot to tlb entry map (increases cache use but may help depending on host cpu arch)
cargo run --release --features ci_clock              # synthetic deterministic CP0 Compare clock (CI/snapshot validator only; loses realtime desktop timing)
cargo run --release --features chd                   # mount .chd disk/CD-ROM images directly (via libchdman-rs); off by default to keep builds light
cargo run --release --features lightning,rex-jit,tlbvmap     # recommended for best speed right now
```

### CHD image support (`--features chd`)

Off by default. When enabled, IRIS can mount `.chd` hard-disk and CD-ROM
images directly without first extracting to raw. Compressed parent CHDs
stay untouched — writes go to a MAME-style `.diff.chd` sidecar.

```
cargo build --release --features chd
```

Without this feature, attempting to mount a `.chd` path returns an
`Unsupported` error; raw images and COW overlays continue to work as
before.

See [HELP.md](HELP.md) for the full rundown: serial ports, monitor console,
NVRAM/MAC address setup, disk image prep, and more.


## JIT compilers

### MIPS JIT (`--features jit`)

Optional Cranelift-based JIT. Compiles hot MIPS basic blocks to native x86_64.
Enable with `--features jit` at build time and `IRIS_JIT=1` at runtime.

Three tiers: blocks start ALU-only (registers + branches), promote to
Loads (+ memory reads), then Full (+ stores) based on stable execution. Probe
interval is adaptive. Hot block profiles persist across sessions.

```
IRIS_JIT=1 cargo run --release --features jit
```
| Variable | Default | Description |
|----------|---------|-------------|
| `IRIS_JIT` | 0 | Enable JIT (1) or interpreter-only (0) |
| `IRIS_JIT_MAX_TIER` | 2 | Cap tier: 0=ALU, 1=Loads, 2=Full |
| `IRIS_JIT_VERIFY` | 0 | Run each block through interpreter and compare (debug) |
| `IRIS_JIT_PROBE` | 200 | Base probe interval (steps between cache checks) |

### REX3 graphics JIT (`--features rex-jit`)

Cranelift-based JIT for the REX3 graphics chip draw pipeline. Compiles a
specialized native "shader" per unique (DrawMode0, DrawMode1) pair, inlining the
entire draw loop — coordinate stepping, clipping, shade DDA, pattern advance —
into a single function. Shaders compile in the background on first use; compiled
profiles persist across sessions for instant warm-up on next boot.

```
cargo run --release --features rex-jit
```

## Copy-on-write disk overlay

Protects disk images from corruption during development and testing. The base
`.raw` file is opened read-only and writes go to a sparse overlay file. Kill
the emulator whenever you want. Delete the overlay to reset to the clean base.

Enable in `iris.toml`:
```toml
[scsi.1]
path = "scsi1.raw"
cdrom = false
overlay = true
```

Writes go to `scsi1.raw.overlay`. Monitor commands:
- `cow status` - show dirty sector count
- `cow commit` - merge overlay into base image (permanent)
- `cow reset` - discard all overlay writes


## Snapshots and rollback

Capture the full machine state — RAM, every device, plus the COW overlay — into
`saves/<name>/`, and restore it later. CPU, MC, IOC, HPC3, REX3, RTC, EEPROM,
SCSI controller, and the Seeq Ethernet chip all round-trip. Current schema
version is 3: postcard-encoded binary device state plus content-addressable
chunked RAM under `saves/.cas/`. A second snapshot taken from the same parent
adds **zero bytes** to disk for any RAM region that didn't change — same
storage model as Docker layers.

From the interactive monitor (`telnet 127.0.0.1 8888`):
```
save base/desktop          # writes saves/base/desktop/
load base/desktop          # restore everything (RAM, devices, disk overlay)
```

From `iris-ci` (the wrapper — see CI socket section below):
```bash
iris-ci save base/desktop
iris-ci restore base/desktop          # full disk-backed reload (~150 ms cold)
iris-ci rollback                      # in-memory rewind to last restore (~40 ms)
iris-ci diff base/desktop tests/grep  # what changed: devices, RAM chunks, COW sectors
iris-ci validate base/desktop -n 1000000  # bit-deterministic re-execution check (build with --features ci_clock)
iris-ci tree                          # snapshot parent-chain hierarchy
iris-ci gc                            # sweep CAS chunks no kept snapshot references
iris-ci pull http://reg/snapshots/base   # fetch a snapshot from another machine
```

Two restore tiers:
- **`restore <name>`** — full disk-backed reload. ~150 ms. Use after a hard
  reset or to switch to a different snapshot.
- **`rollback`** — in-memory rewind to the last `restore` checkpoint. ~40 ms,
  no disk I/O. Use this in tight inner test loops where you keep returning to
  the same starting state.

Reflinks are used on APFS / btrfs / xfs so capturing a snapshot of a 4 GB disk
image takes <10 ms and uses ~18 MB of actual disk.

See [CHANGELOG.md](CHANGELOG.md) for the full feature set, and
[manual_test_runbook.md](manual_test_runbook.md) for a copy-paste tour.


## CI control socket and `iris-ci`

`--ci` enables a Unix-socket control plane for headless automation, plus a
small in-process serial backend so the harness can drive the IRIX console
directly. The default socket path is `/tmp/iris.sock`.

```
cargo run --release --features lightning -- --ci
```

`cargo build` produces a companion binary, `iris-ci`, that's the **canonical
way** to drive the socket. Don't bother with raw `nc` + JSON unless you're
debugging the wrapper itself.

```bash
# In one terminal: launch iris (Newport window opens, --ci is just an extra channel)
./target/release/iris --ci

# In another terminal: drive it
./target/release/iris-ci boot          # PROM menu → IRIS console login (one cmd)
./target/release/iris-ci login         # send root + dismiss vt100 prompt + wait #
./target/release/iris-ci run 'ls /'    # send shell command, get stdout + exit code
./target/release/iris-ci save base/multiuser
./target/release/iris-ci put localfile.tar   # copy file into guest, no bs=512 math
./target/release/iris-ci get /tmp/out --to ./out.tar
./target/release/iris-ci diff base mutated   # per-device + chunk + cow-sector deltas
./target/release/iris-ci tree
./target/release/iris-ci script tests/scenario.iris   # batch-run a sequence of cmds
```

Run `iris-ci --help` for the full list, or `iris-ci <subcmd> --help` for any
subcommand. Every operation has a typed clap arg — no JSON quoting, no
hand-managed timeouts.

For automation that doesn't want to depend on `iris-ci`, the underlying socket
protocol is newline-delimited JSON; `cmd` and `args` per request, `{ok, data,
error}` per response. See `src/ci.rs` for the dispatch table.


## Scratch volume — file injection without networking

A SCSI device with `scratch = true` is a host-controlled raw block device for
pushing files into the guest (and pulling artifacts back out) without bringing
up NFS or anything else. iris pre-formats the underlying file with a minimal
SGI Volume Header on first run, and exposes it inside IRIX as
`/dev/rdsk/dks0d2s0`.

Enable in `iris.toml`:
```toml
[scsi.2]
path    = "scratch.raw"
cdrom   = false
overlay = false
scratch = true
size_mb = 64
```

The easy way (via `iris-ci`):
```bash
iris-ci put localfile.tar                 # copies host file into the guest
iris-ci get /tmp/output.log --to ./out.log  # pulls a guest file out
```

`iris-ci put`/`get` handle the IRIX `dd bs=512` sector-alignment quirk
transparently — they compute the right block count from the host file size,
issue the right `dd` recipe to the guest, and truncate to the original byte
length on the receiving end.

Manual/raw paths (if you want to drive `dd` yourself):
- Reads MUST use `bs=512` (or any 512-multiple); `bs=64` returns "I/O error".
- Writes must be padded to `bs`; add `conv=sync` for short inputs.
- Inside IRIX: `dd if=/dev/rdsk/dks0d2s0 bs=512 | tar xf -`


## Input

Click the window to grab mouse and keyboard. Right Ctrl releases the grab.
Mouse and keyboard use standard PS/2 emulation through the IOC.

**Note:** Alt-tabbing away from the window can garble keyboard input in IRIX
terminal apps. Use `telnet 127.0.0.1 2323` (with port forwarding configured)
for a clean terminal instead.


## Rules

The `rules/` directory contains hard-won lessons from debugging the JIT and
getting IRIX running. These are meant for both humans and AI assistants working
on the codebase.

- `rules/jit/` - dispatch architecture, store compilation, sync, verify mode, probe tuning
- `rules/irix/` - networking config, keyboard quirks, csh + scratch raw-device gotchas
- `rules/testing/` - disk image handling, avoiding filesystem corruption
- `rules/snapshot/` - snapshot binary format, scratch-volume conventions, round-trip tests, CI overlay paths, **iris-ci as the canonical CI interface**

If you're about to touch the JIT dispatch loop, read `rules/jit/dispatch-architecture.md`
first. It'll save you a few days.


## License

BSD 3-Clause

## Whodunnit?

Dominik Behr
