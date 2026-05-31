# GUI mouse integration — current approach, and the Snow absolute-mouse pattern

Status: reference / design analysis. Captures why iris-gui uses pointer
**capture** for the framebuffer, and why the seamless absolute-mouse trick used
by the `snow` Macintosh emulator does **not** port to IRIX without significant
new machinery. Read this before proposing "make the mouse seamless like snow."

## Current iris-gui approach: capture (grab + hide)

The guest's PS/2 mouse is **relative** — it reports motion deltas, and IRIX/X11
draws its own pointer with its own acceleration. A relative guest pointer can
never stay pixel-aligned with a *visible* host cursor; the two drift, and the
host→guest sensitivity is wrong on top of that.

So iris-gui uses the standard emulator model (mirroring `src/ui.rs`):

- **Click the framebuffer to capture.** On capture we hide the host cursor and
  lock it in place (`egui::ViewportCommand::CursorGrab(CursorGrab::Locked)` +
  `CursorVisible(false)`). Only the guest's own pointer is visible, so there is
  nothing to misalign.
- **Motion uses raw deltas.** eframe forwards `winit DeviceEvent::MouseMotion`
  as `egui::Event::MouseMoved(delta)` regardless of grab mode
  (`eframe .../glow_integration.rs` → `egui_winit::on_mouse_motion`). We read
  those deltas and feed them straight to the PS/2 controller — natural 1:1
  feel, no scaling, no warp-to-center, no edge-piling.
- **Esc (or focus loss) releases.** Input is gated on capture: while captured,
  keyboard + mouse go to the guest; while not, they stay with egui (so menu
  clicks and config-side-panel typing don't leak into IRIX).

Implementation: `iris-gui/src/input.rs` (`pump`, `release_capture`,
`force_release`); capture is also force-released when the emulator stops so the
host cursor can't get stuck hidden.

> Note: iris's `mouseabs` cargo feature is **misnamed** — it is still grab +
> warp-to-center + relative deltas (`src/ui.rs:532`), *not* absolute
> positioning. There is no hidden absolute backend to tap.

## What `snow` does (the absolute pattern)

`snow` (sibling repo `../snow`, a classic Macintosh emulator) gets seamless,
capture-free, 1:1 mouse alignment via an **absolute** mode that bypasses the
emulated mouse hardware entirely:

- `mouse_update_abs(x, y)` writes the host cursor position directly into classic
  Mac OS **low-memory globals**: `MTemp` (MouseTemp) and `RawMouse`, then sets
  the `CrsrNew` flag. See `core/src/mac/compact/bus.rs:476` (and the Mac II
  variant in `core/src/mac/macii/bus.rs`).
- Mac OS polls those globals every tick and "jumps" its cursor to the new
  position. The ADB mouse (`core/src/mac/adb/mouse.rs`) stays relative but is
  sidestepped in absolute mode.
- The frontend exposes a `MouseMode { Absolute, RelativeHw, Disabled }` seam and
  calls `update_mouse(abs_p, rel_p)` (`frontend_egui/src/emulator.rs:434`),
  dispatching `MouseUpdateAbsolute { x, y }` vs `MouseUpdateRelative { .. }`.

It works because **classic Mac OS exposes a stable, documented, memory-mapped
cursor position you may overwrite, and cooperatively re-reads it.**

## Why it does not port to IRIS/IRIX cheaply

IRIX has no equivalent of that mechanism:

- **No fixed mouse globals.** IRIX is Unix + X11. Pointer position lives in the
  X server's dynamically-allocated state at addresses that vary per boot/build —
  there is no constant to poke like `MTemp`.
- **The cursor is a hardware sprite** programmed by the X server through
  REX3/VC2/RAMDAC, and X derives pointer position from *relative* input-device
  events plus its own acceleration curve.
- **No "set absolute pointer via memory" convention.** X's supported absolute
  paths are the input protocol (absolute valuators / XInput) or
  `XWarpPointer`/XTEST — none of which the emulated SGI PS/2-style mouse exposes.

## What a Snow-like absolute mode would actually require here

One of:

1. **Emulate an absolute pointing device** IRIX already has a driver for (e.g. a
   tablet/touch valuator on the input bus) and feed normalized coordinates —
   new device emulation, depends on IRIX driver support.
2. **A guest-side agent** that calls `XWarpPointer` from coordinates passed over
   a channel — requires installing software inside the guest.
3. **A feedback hack**: locate the X server's pointer coordinates in guest RAM
   at runtime and synthesize relative deltas toward the host position —
   fragile, version-specific, not "without altering much."

None of these is a small port.

## Recommendation

Capture is the correct, standard approach for an X11 guest — it is what
SGI/Unix emulators do, and what snow itself falls back to (`RelativeHw`). The
one piece genuinely worth borrowing from snow is its clean frontend seam: a
`MouseMode` enum + `update_mouse(abs, rel)`. Adopting that abstraction now
(even with only relative/capture wired up) would make options 1 or 2 drop-in
later, without committing to the absolute backend today.
