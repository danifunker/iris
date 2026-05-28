# Installing IRIX 6.5.22 on iris (from scratch)

Adapted from <https://sgi.neocities.org/installguide> (which targets MAME)
with the corrections and shortcuts we found running it through iris.

## What you need

- `iris` built with `--features chd,camera,lightning` (the WD33C93 +
  HPC3 fixes for the miniroot install path are part of mainline now;
  see `rules/irix/miniroot-install-hang-scsi0-dma-irq-storm.md`).
- An **empty** `irix65.chd` (any size ≥ 4 GB) at SCSI ID 1. Create with
  `chdman` (Homebrew: `brew install rom-tools`):

  ```bash
  # 20 GB uncompressed CHD — sparse on disk (~20 MB used until written)
  chdman createhd -o irix65.chd -s 21474836480 -ss 512 -c none
  ```

  iris auto-creates a `.diff.chd` sidecar next to compressed parents;
  uncompressed CHDs are written in place, which is what you want for
  a fresh install (no sidecar accumulation).
- `prom.bin` is **not** required — iris falls back to an embedded PROM
  image when the file is missing. You'll see one line at startup:
  `Warning: Could not read PROM file 'prom.bin': ... — using embedded PROM`.
- The six 6.5.22 ISOs (filenames vary by media kit — the names below
  are the SGI 6.5.22 release-kit labels, which is what's typically on
  disk under `irix/`):
  - Installation Tools and Overlays (1 of 3) ← bootable (miniroot +
    fx.ARCS + sashARCS) — the "Overlay 1" the recipe refers to
  - Overlays (2 of 3)
  - Overlays (3 of 3)
  - IRIX 6.5 Applications November 2003
  - IRIX 6.5 Foundation 1
  - IRIX 6.5 Foundation 2
  - IRIX Development Foundation 1.3 (optional — not in the recipe below)

  If your filenames differ from what's in the `iris.toml` snippet
  below, edit the `discs = [...]` list to match the actual paths.
  A clean template lives at `iris-irix65.toml` in this repo.

## iris-irix65.toml

Keep the install config separate from your day-to-day `iris.toml` —
the install needs a different disk path, the full CD changer order,
and the specific PROM env tweaks, and you don't want any of those
bleeding into normal operation. A ready-to-edit template lives at
the repo root as `iris-irix65.toml`; launch with `--config
iris-irix65.toml --ci --ci-display`.

> ⚠️ **`headless = false` is mandatory for the install.** With
> `headless = true`, iris doesn't map REX3 and miniroot's hinv
> records `GFXBOARD=SERVER`. Inst then applies its
> `RGFXBOARD!=SERVER` tag filters and strips every Newport-specific
> file — `Xsgi`, `gfxinit`, `/hw/gfx` autoconfig, board-specific
> keymaps — while still marking subsystems like `x_eoe.sw.Server`
> and `eoe.sw.gfx` as Installed. The system boots multi-user but
> xdm has nothing to launch, and there is no in-place fix short of
> redoing the install. See section 10 for the gory details. Pair
> `headless = false` with `--ci --ci-display` so iris-ci stays
> reachable while iris draws the Newport window miniroot needs to
> see.

The disk goes at SCSI 1, CD changer at SCSI 4 with Overlay 1 active
and the rest in cycle order. Keep `[vino] source = "test_pattern"`
for the install (avoid the 30 fps camera interrupt rate while
booting); flip to `"camera"` after.

```toml
headless    = false
no_audio    = false
banks       = [128, 128, 0, 0]
serial_log  = "irix-install-console.log"

[scsi.1]
path  = "irix65.chd"
cdrom = false

# Adjust the paths below to match your actual ISO filenames.
[scsi.4]
path  = "irix/IRIX 6.5.22 Installation Tools and Overlays (1 of 3).iso"
cdrom = true
discs = [
  "irix/IRIX 6.5.22 Installation Tools and Overlays (1 of 3).iso",
  "irix/IRIX 6.5.22 Overlays (2 of 3).iso",
  "irix/IRIX 6.5.22 Overlays (3 of 3).iso",
  "irix/IRIX 6.5 Applications November 2003.iso",
  "irix/IRIX 6.5 Foundation 1.iso",
  "irix/IRIX 6.5 Foundation 2.iso",
]

[vino]
source = "test_pattern"

[[port_forward]]
proto = "tcp"; host_port = 2323; guest_port = 23; bind = "localhost"
```

## Driving the install

All commands go through `iris-ci`. Boot iris with `--ci --ci-display`
(the `--ci-display` keeps the Newport window visible alongside the
control socket, which is what allows miniroot to detect graphics):

```bash
./target/release/iris --config iris-irix65.toml --ci --ci-display \
    > /tmp/iris.stdout.log 2>&1 &
```

Then in another shell:

```bash
alias ic=./target/release/iris-ci
ic ping     # liveness check
ic start    # CPU thread does not auto-start in --ci mode
```

### Watch prompts with `tools/inst-watch.py`

The install asks 20+ interactive questions across a couple of hours.
Driving it with chained `serial-wait` calls is fragile (stale buffer
matches, prompt-shape variants, race against `serial-read`). Instead
run `tools/inst-watch.py` as a persistent background watcher — it
tails `irix-install-console.log` and emits one classified event per
stall (`mkfs_confirm`, `numbered_choice`, `install_software_from`,
`yn_confirm`, `inst_ready`, `cd_swap`, `restart_confirm`, etc.) with
a hint for how to respond. From Claude Code:

```
Monitor: tools/inst-watch.py --log irix-install-console.log --quiet-secs 6 --json
```

From a shell, just run it in the foreground and react to each event
line as it appears. The classifier is in the script's docstring;
add new prompt shapes there if you find one it doesn't recognise.

**Avoid `serial-read` while a `serial-wait` is in flight on the same
socket** — they race and you'll see `connect /tmp/iris.sock: Resource
temporarily unavailable (os error 35)`. Pick one: drive entirely
through `inst-watch.py` events, or use only `serial-wait` between
sends.

**`tail -F … | grep` won't see prompts that have no trailing newline.**
The `Restart? { (y)es, (n)o, (sh)ell, (h)elp }:` line and a few others
are written without a `\n` because Inst is waiting for input on the
same line. `grep --line-buffered` still buffers until end-of-line, so a
`Monitor: tail -F | grep "Restart"` will silently miss the prompt and
only fire after you've already responded (the keystroke produces the
newline that flushes the line). Use `iris-ci serial-wait "Restart"`
for prompts of this shape — it scans the byte stream, not lines.
Prompts with this gotcha that have bitten this install: the `Restart?`
prompt above, `Are you sure? [y/n] (n):`, `Block size of filesystem
512 or 4096 bytes?`, `Install software from: [/CDROM/dist]`, fx's
`fx> ` and `fx/label/create> ` prompts, and the PROM monitor `>> `.

### 1. PROM env (one-time, with a small chicken-and-egg)

A fresh iris launch reads `nvram.bin`. If the file is missing or its
checksum is bad iris prints `NVRAM checksum is incorrect:
reinitializing.` on the serial console and resets the PROM env to
defaults — including `ConsoleIn=serial(0)`, but the PROM at that
point hasn't latched onto serial yet and `console` is unset (effective
`console=g`). With `headless = false` the graphics console takes
priority and **the serial channel is dead-eared until you've
explicitly set `console=d` in nvram and persisted it**. You can't do
that from the serial console because the PROM isn't listening.

So the bootstrap is a two-phase dance:

```bash
# Phase A: seed nvram via a one-shot headless boot so the serial
# console is the only console — sed the toml or use a separate
# config; restore after.
sed -i.bak 's/^headless    = false/headless    = true/' iris-irix65.toml
./target/release/iris --config iris-irix65.toml --ci \
    > /tmp/iris.stdout.log 2>&1 &
ic ping; ic start

# Wait for the System Maintenance Menu. The exact prompt is "Option?"
# (NOT "Stop for Maintenance" — that string never appears in iris's
# embedded-PROM serial output; the menu draws straight to "Option?").
ic serial-wait --timeout 60 "Option?"

ic serial-send "5"                # Command Monitor
ic serial-wait --timeout 10 ">> " # PROM monitor prompt
ic serial-send "setenv -f eaddr 08:00:69:de:ad:01"
ic serial-wait --timeout 10 ">> "
ic serial-send "setenv -f SystemPartition scsi(0)disk(1)rdisk(0)partition(8)"
ic serial-wait --timeout 10 ">> "
ic serial-send "setenv -f OSLoadPartition scsi(0)disk(1)rdisk(0)partition(0)"
ic serial-wait --timeout 10 ">> "
ic serial-send "setenv -f console d"   # routes output to serial
ic serial-wait --timeout 10 ">> "
ic rtc-save                            # persist nvram.bin — REQUIRED
ic quit

# Phase B: real install, headless=false so REX3 is mapped.
sed -i.bak 's/^headless    = true/headless    = false/' iris-irix65.toml
./target/release/iris --config iris-irix65.toml --ci --ci-display \
    > /tmp/iris.stdout.log 2>&1 &
ic ping; ic start

# The default PROM env has AutoLoad=Yes, so the PROM tries to chain-load
# sash from the (still unformatted) disk before falling through to the
# Maintenance Menu. You'll see:
#
#   No volume header on device: scsi(0)disk(1)rdisk(0)partition(8)/sash.
#   Unable to boot; press any key to continue:
#
# Send an empty line to advance past it, *then* wait for "Option?".
ic serial-wait --timeout 60 "Unable to boot"
ic serial-send ""
ic serial-wait --timeout 30 "Option?"
```

Once Phase A's `rtc-save` is in place, the Phase B PROM picks up
`console=d` at init and routes serial properly. If you happen to
have inherited an nvram.bin from an older `headless = true` run
where the *previous* PROM init flooded `MC: GIO Timeout at
1f0f1338`, those events stop accruing once the menu is up and don't
recur this session.

### 2. Label the disk via fx (one-time per fresh disk)

```bash
ic serial-send "5"
ic serial-send "boot -f dksc(0,4,8)sashARCS dksc(0,4,7)stand/fx.ARCS --x"
# accept three defaults (device-name dksc, ctlr 0, drive 1)
ic serial-send ""; ic serial-send ""; ic serial-send ""
# label/create/all  →  ..  →  sync  →  ..  →  exit
ic serial-send "l"; ic serial-send "c"; ic serial-send "a"
ic serial-send ".."; ic serial-send "sync"
ic serial-send ".."; ic serial-send "exit"
```

### 3. Install System Software → miniroot

```bash
ic serial-send "2"                # PROM "Install System Software"
ic serial-send ""                 # accept default Local CD-ROM 4
ic serial-send ""                 # confirm CD inserted (Overlay 1)
# wait ~2 min for miniroot kernel boot + Inst 4.1 Main Menu
ic serial-wait --timeout 300 "Inst>"
```

If the disk is truly empty (no XFS yet), miniroot will ask
`Make new file system on /dev/dsk/realroot [yes/no/sh/help]:` — answer
`yes`, then `y` to confirm, then **`4096`** for block size (the magic
number for ≥ 4 GB disks).

### 4. Turn off pagination and conflict-rules **first**

```bash
ic serial-send "set page_output off"     # no more "more?" pages
ic serial-send "set rulesoverride on"    # auto-resolve conflicts
```

These two settings are what makes the rest of the recipe a straight
shot. Without them you'll spend an hour resolving 71 + 62 + ... cascading
conflicts by hand — most of which are old-version-vs-new incompatibilities
inst would happily skip if you let it.

### 5. Load all six install distributions

The order matters because each CD is mounted as `/CDROM/dist` (or
`/CDROM/dist/unbundled` for Overlay 2's variant). Eject between each
with `iris-ci cdrom-eject 4`.

```bash
# Overlay 1 (already mounted) — Inst already pointed at /CDROM/dist
ic serial-send "from"
ic serial-send "1"                # /CDROM/dist
# README pager is auto-quit by step 4's "set page_output off" —
# do NOT send "q" (it would be interpreted as quit from the next prompt)
# stream choice (maintenance vs feature) — pick "feature" (2)
# inst-watch classifies this as numbered_choice
ic serial-send "2"
ic serial-wait --timeout 60 "Install software from:"

# Overlay 2  → /CDROM/dist/unbundled  (guide-special path)
ic cdrom-eject 4
ic serial-send "/CDROM/dist/unbundled"
ic serial-wait --timeout 180 "Install software from:"

# Overlay 3 → /CDROM/dist
ic cdrom-eject 4
ic serial-send "/CDROM/dist"
ic serial-wait --timeout 180 "Install software from:"

# Applications → /CDROM/dist
# (license-agreement pager auto-quits because step 4 set page_output off;
#  do NOT send "q" — at the "Install software from:" prompt it means quit
#  and pops you back to Inst> with only 4 CDs scanned)
ic cdrom-eject 4
ic serial-send "/CDROM/dist"
ic serial-wait --timeout 240 "Install software from:"

# Foundation 1 → /CDROM/dist
ic cdrom-eject 4
ic serial-send "/CDROM/dist"
ic serial-wait --timeout 180 "Install software from:"

# Foundation 2 → /CDROM/dist
ic cdrom-eject 4
ic serial-send "/CDROM/dist"
ic serial-wait --timeout 180 "Install software from:"

# Done scanning
ic serial-send "done"
ic serial-wait --timeout 30 "Inst>"
```

### 6. The package-selection recipe

This is the sgi.neocities recipe verbatim — `keep *` then carve out
the standard set. With `rulesoverride on` from step 4, conflicting
older versions get auto-deselected during `go` so you don't have to
hand-resolve.

```bash
ic serial-send "keep *"
ic serial-send "install standard"
ic serial-send "keep java2_plugin.sw32.mozilla_freeware"
ic serial-send "keep inventor_dev.sw.base"
ic serial-send "keep inventor_dev.sw.lib"
ic serial-send "install eoe.sw.fonttools"
ic serial-send "install eoe.sw.uucp"
ic serial-send "install eoe.sw.xlv"
ic serial-send "install ftn_eoe"
ic serial-send "install eoe.sw.spell"
ic serial-send "install inventor_eoe.sw64"
ic serial-send "install ifl_eoe.sw64"
ic serial-send "install dmedia_eoe.sw64"      # "no matches" warning is fine
ic serial-send "install prereqs"
ic serial-send "keep incompleteoverlays"
ic serial-send "go"
```

### 7. The long bit

`go` runs the actual install. Expect 1–2 hours of emulated CPU
chewing through tar streams, writing to the XFS root. The installer
will prompt to swap CDs ("Please insert the `<NAME>` CD."); answer
by cycling the changer until the requested ISO is mounted, then
press Enter:

```bash
# Cycle the SCSI 4 changer until the wanted disc is mounted.
want='Foundation 1'    # match a substring of the requested CD's filename
for i in 1 2 3 4 5 6; do
  cur=$(ic cdrom-eject 4 | sed -E 's/.*new_disc":"([^"]+)".*/\1/')
  echo "now mounted: $cur"
  case "$cur" in *"$want"*) break ;; esac
done
ic serial-send ""
```

If you happen to advance past the requested disc, just keep going —
the changer wraps around. A spurious "You've inserted the incorrect
CD." print is normal while the changer is cycling under the
installer's polling; once the right ISO is mounted the install
resumes automatically.

When `go` finishes you're dropped back at the Inst> prompt — not the
Restart prompt. The success signal in the log is one line:

```
Installations and removals were successful.
```

From Inst>, send `quit`. This triggers:

1. A non-fatal `ERROR: INCOMPATIBLE SUBSYSTEMS INSTALLED` print —
   harmless because step 4 set `rulesoverride on` (the message even
   says "Exit allowed because rulesoverride option is set").
2. A several-minute `Requickstarting ELF files (rqsall(1))` pass that
   walks the entire installed tree (CHD grows another ~50–100 MB).
3. An autoconfig pass (`Automatically reconfiguring the operating
   system.`).
4. Finally, the actual restart prompt — and it's `Restart? { (y)`,
   not the `Restart the system. (y/n)?` the SGI guide shows.

```bash
ic serial-send "quit"
ic serial-wait --timeout 900 "Restart"
ic serial-send "y"
```

### 8. First multi-user boot

After the post-install restart, the PROM goes straight through
`AutoLoad=Yes` and chains into the installed sash → /unix → init.
You don't see the Maintenance Menu at all on this first boot; you'll
see the standard sysadm shell up + a sequence of one-time warnings
that aren't fatal:

```
network: WARNING: IRIS's Internet address is the default.
Using standalone network mode.
UX:mv: ERROR: /etc/resolv.conf - No such file or directory
Warning:  Internet Gateway web server running as root.

IRIS console login:
```

The `UX:mv: ERROR: /etc/resolv.conf` is the standalone-network
bring-up trying to swap in a dhclient resolv.conf and finding none;
it's cosmetic on a clean install. Configure networking later if you
want to silence it.

Log in as `root` (no password yet); `iris-ci login` handles the
`TERM = (vt100)` dismissal automatically:

```bash
ic login
ic run "uname -a; df -k / | tail -1"
```

### 9. Remove the miniroot install stub from the volume header

After the install completes, the SGI volume header on the boot disk
still has an `ide` entry alongside `sash`:

```
IRIS # dvhtool -v list /dev/rdsk/dks0d1vh

Current contents:
        File name        Length     Block #
        ide              343040           2
        sash             343040         672
```

`ide` is the miniroot installer image. Sash checks for it on every boot
and prints

```
It appears that a miniroot install failed.  Either the system is
misconfigured or a previous installation failed.
...
Enter 'c' to continue with no state fixup.
Enter 'f' to fix miniroot install state, and try again
Enter 'a' to abort and return to menu.
```

even though the install actually succeeded — selecting `f` resets the
in-progress flag but the next boot puts it back because the `ide` blob
is still in the volume header. Remove it once and the prompt is gone
for good:

```
IRIS # dvhtool -v delete ide /dev/rdsk/dks0d1vh
IRIS # dvhtool -v list /dev/rdsk/dks0d1vh

Current contents:
        File name        Length     Block #
        sash             343040         672
```

You don't lose anything you'll miss — to reinstall later you'd boot
from a CD anyway, which writes a fresh `ide` for that session.

### 10. Switching to the Indigo Magic Desktop

> ⚠️ **If you ran the install with `headless = true` (per step 0 of
> this guide), the desktop will NOT come up cleanly — you need to
> re-run the install with graphics on.** Inst's tag filters
> (`RGFXBOARD!=SERVER` on `x_eoe.sw.Server`, `x_eoe.sw.Xfonts`,
> `eoe.sw.gfx` server bits, etc.) read the install-time hinv to
> decide what to drop on disk. With `headless = true`, REX3 is
> unmapped, miniroot's hinv records GFXBOARD=SERVER, and inst
> silently strips:
>
> - `/usr/bin/X11/Xsgi` (the actual X server)
> - `/usr/gfx/gfxinit` (graphics-board init)
> - `/hw/gfx/*` autoconfig entries (so the kernel doesn't probe
>   Newport)
> - All board-specific X font and keymap pieces
>
> `versions` will *still* list `x_eoe.sw.Server` as installed
> ("I") — the subsystem is there, but its hardware-tagged files
> aren't. Symptom: xdm runs but no Xsgi is launched, `/var/X11/xdm/
> Xservers` is empty, `hinv` doesn't show a graphics line, and
> `/usr/bin/X11/X` prints `X: /usr/gfx/gfxinit not found`.
>
> **Fix**: rerun the install with `headless = false` (drop step 4's
> `console=d` setenv, leave `console=g`), so miniroot probes graphics
> and the SERVER filter doesn't trip. The rest of the recipe is
> identical. There's no in-place fix from the booted system that's
> simpler than reinstalling — the dropped files span multiple
> subsystems and inst won't repaint them without the hinv override.

The install brought in the desktop stack (`x_eoe`, `desktop_eoe`,
4Dwm, the toolchest) and turned `desktop`/`visuallogin`/`xdm` on by
default. Assuming you did *not* run the install headless, the
remaining switchover steps are:

```bash
# 1) From the IRIX shell: enable the X server + tell the PROM to use
#    graphics console.
ic run "chkconfig windowsystem on"
ic run "nvram console g"

# 2) Persist the emulated NVRAM to nvram.bin on the host. Without this
#    the console=g change lives only in iris's in-memory NVRAM and
#    evaporates when iris exits — the next launch reads stale nvram.bin
#    and you're back to console=d. This applies to any PROM env change
#    made from inside IRIX (nvram(1)) or from the PROM monitor (setenv).
ic rtc-save

# 3) Cleanly halt so XFS journals are flushed, then quit iris.
ic run "halt -y"
ic serial-wait --timeout 60 "Maintenance Menu"
ic quit
```

Now use a non-headless config for the launch. Simplest: in
`iris-irix65.toml`, remove `headless = true` (or set it `false`).
The daily-use `iris.toml` is fine to keep for other disks. Flip
vino back to `source = "camera"` here too if you want the IndyCam
live.

```bash
./target/release/iris --config iris-irix65.toml \
    > /tmp/iris.stdout.log 2>&1 &
```

iris pops a Newport window; the PROM runs through AutoLoad and
chains into the installed kernel; xdm draws the graphical login
manager. (If you want the CI socket alongside the visible window,
launch with `--ci --ci-display` instead — that keeps `iris-ci`
usable at the cost of ~10–15 fps render rate.)

To verify from the host without looking at the window:

```bash
ic login
ic run "ps -ef | grep -E 'Xsgi|4Dwm|toolchest' | grep -v grep"
ic screenshot /tmp/desktop.png   # only with --ci-display
```

## Conflict resolution without `rulesoverride on`

The sgi.neocities recipe assumes `rulesoverride on` so inst silently drops
packages whose prerequisites can't be met from the loaded CDs. That works
but it leaves you with no record of *what* got skipped. The honest path is:

```bash
ic serial-send "set page_output off"
# Skip 'set rulesoverride on'.  Use the parser instead.
```

When `go` returns `ERROR: Conflicts must be resolved.`, dump the
conflicts list, run it through `tools/inst-resolve.py`, and pipe the
result back in:

```bash
ic serial-send "conflicts"
sleep 8
ic serial-read > /tmp/conf.txt
python3 tools/inst-resolve.py < /tmp/conf.txt > /tmp/resolve.txt 2> /tmp/skipped.txt
cat /tmp/skipped.txt    # which packages are getting skipped + why
while IFS= read -r line; do
  ic serial-send "$line"
  sleep 2
done < /tmp/resolve.txt
ic serial-send "go"
```

The resolver picks the highest-letter option (`c` over `b` over `a`)
whose text doesn't mention "from an additional distribution" or
"Open new distribution" — i.e. an option whose prereqs are on one of
the already-scanned distributions. Falls back to `a` (don't install)
only when nothing else works, and *reports* every fallback.

If `tools/inst-resolve.py` reports any fallback at all, that's your
signal that the install isn't complete for the recipe as written —
some product wanted a prereq that isn't on any loaded CD.

## Known missing CD: 6.5.22-era Foundations 1.1

With the 7 CDs in `irix/6.5.22/` listed above, `tools/inst-resolve.py`
falls back on a ~40-package set whose required base versions are
`1274627340` (e.g. `eoe.sw.base`, `eoe.sw.gfx`, `x_eoe.sw.eoe`, etc.).
That version is the **2002–2003 Foundations 1.1 refresh** that SGI
shipped alongside the 6.5.22 media kit. Our `IRIX-6.5-Foundation1.iso`
is the original 1998 Foundation 1 with version `1274627333`, which is
too old for the 6.5.22 overlays.

We have a `Foundations 1.1.iso` in `irix/6.5.29/` but the 6.5.22
miniroot's inst rejects it with `ERROR: This software distribution is
not meant to install on the version of IRIX currently running on this
machine.` — SGI rev'd it for each release wave.

**To get a fully clean install you need the IRIX 6.5 Foundations 1.1
CD from the 6.5.22 media kit specifically** (look for an ISO dated
late 2002 / 2003 with the "1.1" label, not the 6.5.29-era one).

Without it, the install still completes a *core* 6.5.22 base
(Overlays + Apps that don't depend on the missing base versions) but
loses these optionals:

- `eoe.sw.{base,gfx,gifts_perl}` overlay updates
- `x_eoe.sw.{eoe,plugin}` overlay updates
- `motif21_dev`, `ViewKit21_dev`
- `cosmoplayer`, `netscape`, `mozilla`, `appletalk`, `arraysvcs`
- `performer_eoe.sw.performer` (demo)
- `media_warehouse` viewers
- `il_eoe.sw.{c++,vk}`, `ifl_eoe.sw.c++`
- `webviewer`, `infosearch`, `sysmon` desktop pieces

## Iris-specific gotchas

- **`headless = false` + `console=d` is the correct install combo,
  not `headless = true`.** Earlier versions of this guide ran the
  install with `headless = true` to avoid needing a window — that
  produces a degraded install (see the warning at the top of section
  10). Use `headless = false` so REX3 is mapped (miniroot probes
  Newport correctly and inst's `RGFXBOARD!=SERVER` filters don't
  strip Xsgi etc.), and keep `console=d` so the inst dialog routes
  to ttyd1 where `iris-ci` can drive it. With REX3 mapped, the old
  failure mode here (`MC: GIO Timeout at 1f0f1338` flood, UTLB miss
  at PC `0x97f9c39c`/`0x1c`) doesn't happen: the PROM finds a real
  graphics device to back the `console=g`-era pointers even though
  we're routing output to serial. Setting `console=d` mid-session
  with `setenv -f` still requires an `rtc-save` (see the next bullet)
  so the value survives an iris restart.
- **fx defaults** are correct on iris; `label/create/all` writes a
  standard SGI volume header with a root partition that mkfs will
  later format as XFS.
- **`scsi eject 4`** vs `iris-ci cdrom-eject 4` are the same thing.
  Use the iris-ci form so it's scriptable.

## Driving the installer reliably

The Inst program is interactive and emits prompts that don't always
match `serial-wait` patterns cleanly. Two things bite hard:

1. **Catalog rescans at the same path REPLACE, they don't accumulate**
   — but only if you've already made selections. The "from" loop in
   step 5 must scan all six CDs *before* any `install`/`keep` command
   runs. Once a selection exists, re-entering the same path
   (`/CDROM/dist`) inside `from` prompts:

   ```
   There are products marked for installation or removal.
   Switching distributions will cause the selections to be lost.
   Do you really want to switch distributions? (y/n)
   ```

   Answering `y` discards the prior catalog at that path. The doc's
   `from`-loop is structured so all six CDs are scanned before any
   selection — keep it that way. If you `done` out of the `from` loop
   too early and then `from` again, you will hit this prompt for every
   CD you swap in at `/CDROM/dist` and end up with only the last CD's
   catalog. Symptom: `install eoe.sw.fonttools` returns "No matches"
   for core eoe packages from Overlay 1.

2. **`serial-wait` for `"Install software from:"` matches stale
   buffer**, since that prompt persists across CD-load iterations. Wait
   for `"100% Done\\."` (emitted exactly once per scan completion)
   between CDs instead. And never drain the buffer with `serial-read`
   while a `serial-wait` is in flight against the same socket — it
   competes with the wait's stream and the wait will time out even
   after the pattern has appeared.

3. **Catch *all* prompt shapes proactively.** A narrow `tail -F | grep
   -E "100% Done|Install software from:"` will miss `(y/n)`,
   `Please enter a choice [1]:`, license-agreement `Press <Enter>`,
   and `Inst>` returns. Use a wider filter that emits on any of:
   `100% Done`, `Install software from:`, `Inst>` at EOL, `(y/n)`,
   `Press` + `Enter`, `Please enter`, `[yes/no`, `Restart the system`,
   `Insert.*press`, `ERROR:`, `Conflicts`, `PANIC|Exception`, lines
   ending with `?`.

4. **README/license pagers**. The Applications CD prints a multi-page
   license agreement during scan. With `set page_output off` (step 4)
   the pager is auto-quit and *no* `q` keystroke is needed — sending
   `q` afterwards is interpreted as `quit` at the next prompt and pops
   you back to `Inst>` prematurely. Earlier versions of this guide
   recommended sending `q` after the Apps load; don't.
