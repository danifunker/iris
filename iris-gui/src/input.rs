//! Translate egui keyboard / pointer events into iris PS2 controller writes.
//!
//! ## Mouse / keyboard capture
//!
//! The guest's PS/2 mouse is *relative*: it reports motion deltas, and IRIX
//! draws its own pointer with its own acceleration. There is no way to keep a
//! relative guest pointer pixel-aligned with a visible host cursor — they
//! drift. So we adopt the standard emulator model (mirroring `src/ui.rs`):
//! **click the framebuffer to capture.** On capture we hide the host cursor
//! and lock it in place (`CursorGrab::Locked`); raw motion then arrives as
//! `egui::Event::MouseMoved` deltas (eframe forwards `DeviceEvent::MouseMotion`
//! regardless of grab), which we feed straight to the guest. Only the guest's
//! own pointer is visible, so there is nothing to misalign. **Ctrl+Alt+Esc
//! releases** (Alt is the Option key on macOS); a chord rather than bare Esc
//! so plain Esc still reaches the guest.
//!
//! While captured we also forward keyboard input to the guest; while *not*
//! captured we forward nothing, so menu clicks and typing into the config
//! side panel stay with egui.
//!
//! The framebuffer panel calls `pump(...)` each frame with the rect the REX3
//! image occupies in screen space (used only to decide where a capturing
//! click counts).

use egui::{CursorGrab, Event, Key, Modifiers, PointerButton, Rect, ViewportCommand};
use iris::ps2::Ps2Controller;
use winit::keyboard::KeyCode;

pub struct InputState {
    last_mods: Modifiers,
    last_buttons: u8,         // bit0=L, bit1=R, bit2=M
    /// True while the host cursor is grabbed and input is routed to the guest.
    pub captured: bool,
}

impl Default for InputState {
    fn default() -> Self {
        Self { last_mods: Modifiers::NONE, last_buttons: 0, captured: false }
    }
}

pub fn pump(ctx: &egui::Context, fb_rect: Rect, ps2: &Ps2Controller, state: &mut InputState) {
    // Collect everything we need inside the input borrow, then act afterwards
    // (sending viewport commands / PS2 writes outside the `input()` closure).
    let mut want_enter = false;
    let mut want_release = false;
    let mut dx = 0.0f32;
    let mut dy = 0.0f32;
    let mut buttons = state.last_buttons;
    let mut mods = state.last_mods;
    let mut keys: Vec<(KeyCode, bool)> = Vec::new();

    ctx.input(|i| {
        if !state.captured {
            // Not captured: the only thing we care about is a primary click
            // inside the framebuffer, which grabs input. Everything else is
            // left to egui (menus, config side panel, …).
            if i.pointer.button_pressed(PointerButton::Primary) {
                if let Some(p) = i.pointer.interact_pos().or_else(|| i.pointer.latest_pos()) {
                    if fb_rect.contains(p) { want_enter = true; }
                }
            }
            return;
        }

        // Captured. Ctrl+Alt+Esc (Alt == Option on macOS) — or losing window
        // focus (alt-tab) — releases. Using a chord rather than bare Esc lets
        // plain Esc reach the guest.
        if (i.key_pressed(Key::Escape) && i.modifiers.ctrl && i.modifiers.alt) || !i.focused {
            want_release = true;
            return;
        }

        mods = i.modifiers;

        for ev in &i.events {
            match ev {
                // Raw relative motion (eframe → DeviceEvent::MouseMotion).
                Event::MouseMoved(d) => { dx += d.x; dy += d.y; }
                Event::Key { key, pressed, .. } => {
                    if let Some(kc) = map_key(*key) { keys.push((kc, *pressed)); }
                }
                _ => {}
            }
        }

        let mut b = 0u8;
        if i.pointer.button_down(PointerButton::Primary)   { b |= 0x01; }
        if i.pointer.button_down(PointerButton::Secondary) { b |= 0x02; }
        if i.pointer.button_down(PointerButton::Middle)    { b |= 0x04; }
        buttons = b;
    });

    if want_enter {
        state.captured = true;
        // Anchor modifier/button state so we don't synth a spurious press for
        // a key/button already held at capture time.
        state.last_mods = ctx.input(|i| i.modifiers);
        state.last_buttons = 0;
        ctx.send_viewport_cmd(ViewportCommand::CursorVisible(false));
        ctx.send_viewport_cmd(ViewportCommand::CursorGrab(grab_mode()));
        return;
    }

    if want_release {
        release_capture(ctx, ps2, state);
        return;
    }

    // ---- modifiers: diff previous → current, synth press/release. ----
    let m = mods;
    if m.shift && !state.last_mods.shift { ps2.push_kb(KeyCode::ShiftLeft, true); }
    if !m.shift && state.last_mods.shift { ps2.push_kb(KeyCode::ShiftLeft, false); }
    if m.ctrl  && !state.last_mods.ctrl  { ps2.push_kb(KeyCode::ControlLeft, true); }
    if !m.ctrl  && state.last_mods.ctrl  { ps2.push_kb(KeyCode::ControlLeft, false); }
    if m.alt   && !state.last_mods.alt   { ps2.push_kb(KeyCode::AltLeft, true); }
    if !m.alt   && state.last_mods.alt   { ps2.push_kb(KeyCode::AltLeft, false); }
    if m.mac_cmd && !state.last_mods.mac_cmd { ps2.push_kb(KeyCode::SuperLeft, true); }
    if !m.mac_cmd && state.last_mods.mac_cmd { ps2.push_kb(KeyCode::SuperLeft, false); }
    state.last_mods = m;

    // ---- key events ----
    for (kc, pressed) in keys { ps2.push_kb(kc, pressed); }

    // ---- mouse: raw per-frame delta + button diff. ----
    let (mdx, mdy) = (dx as i32, dy as i32);
    if buttons != state.last_buttons || mdx != 0 || mdy != 0 {
        send_mouse_packet(ps2, buttons, mdx, -mdy); // PS/2 Y axis is up-positive
        state.last_buttons = buttons;
    }
}

/// Pick the cursor-grab mode winit actually supports on this platform/session.
///
/// winit's two grab modes are not portable: `Locked` (lock the cursor in place
/// and deliver relative motion) is supported on macOS and Wayland but **not
/// X11**, while `Confined` (keep the cursor inside the window) is supported on
/// X11 and Windows but **not macOS**. egui-winit does no fallback — it just
/// logs the error — so a blanket `Locked` silently fails on X11, leaving the
/// cursor un-grabbed: it drifts off-window, loses focus, and capture drops.
///
/// We rely on raw `DeviceEvent::MouseMotion` deltas (which arrive regardless of
/// grab mode), so `Confined` is sufficient on X11 — it just stops the cursor
/// escaping the window. Detect Wayland via `WAYLAND_DISPLAY`; otherwise assume
/// X11 on Linux. macOS/Windows keep `Locked`.
fn grab_mode() -> CursorGrab {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            CursorGrab::Locked
        } else {
            CursorGrab::Confined
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        CursorGrab::Locked
    }
}

/// Release a capture: show + ungrab the host cursor and lift any modifiers
/// we'd synthesised, so the guest doesn't see stuck keys. Safe to call when
/// not captured (no-op). Used both for Esc/focus-loss and when the emulator
/// stops while the framebuffer still had the grab.
pub fn release_capture(ctx: &egui::Context, ps2: &Ps2Controller, state: &mut InputState) {
    if !state.captured { return; }
    if state.last_mods.shift   { ps2.push_kb(KeyCode::ShiftLeft, false); }
    if state.last_mods.ctrl    { ps2.push_kb(KeyCode::ControlLeft, false); }
    if state.last_mods.alt     { ps2.push_kb(KeyCode::AltLeft, false); }
    if state.last_mods.mac_cmd { ps2.push_kb(KeyCode::SuperLeft, false); }
    state.captured = false;
    state.last_mods = Modifiers::NONE;
    state.last_buttons = 0;
    ctx.send_viewport_cmd(ViewportCommand::CursorVisible(true));
    ctx.send_viewport_cmd(ViewportCommand::CursorGrab(CursorGrab::None));
}

/// Ungrab the cursor without touching the guest (it may already be gone).
/// Called when the emulator stops while the framebuffer still held capture,
/// so the host cursor doesn't stay hidden/locked. No-op when not captured.
pub fn force_release(ctx: &egui::Context, state: &mut InputState) {
    if !state.captured { return; }
    state.captured = false;
    state.last_mods = Modifiers::NONE;
    state.last_buttons = 0;
    ctx.send_viewport_cmd(ViewportCommand::CursorVisible(true));
    ctx.send_viewport_cmd(ViewportCommand::CursorGrab(CursorGrab::None));
}

/// Build and dispatch one PS/2 mouse packet. Mirrors `src/ui.rs:646–658`:
/// byte 0 bit3 always 1, bits 2..0 are buttons (M/R/L), bit 4 = X sign,
/// bit 5 = Y sign, bits 6/7 = X/Y overflow.
fn send_mouse_packet(ps2: &Ps2Controller, buttons: u8, dx: i32, dy: i32) {
    // Clamp to the 9-bit signed range expected by the protocol. Real
    // drivers split large moves; that's fine to skip here because egui
    // delivers small per-frame deltas.
    let sx = dx.clamp(-256, 255);
    let sy = dy.clamp(-256, 255);
    let mut b0 = 0x08 | (buttons & 0x07);
    if sx < 0 { b0 |= 0x10; }
    if sy < 0 { b0 |= 0x20; }
    if sx < -256 || sx > 255 { b0 |= 0x40; }
    if sy < -256 || sy > 255 { b0 |= 0x80; }
    ps2.push_mouse_packet(b0, sx as u8, sy as u8);
}

/// egui::Key → winit::keyboard::KeyCode. Returns None for keys iris's
/// scancode mapper doesn't recognise (we just drop them rather than
/// inventing a fallback).
fn map_key(k: Key) -> Option<KeyCode> {
    Some(match k {
        // Letters
        Key::A => KeyCode::KeyA, Key::B => KeyCode::KeyB, Key::C => KeyCode::KeyC,
        Key::D => KeyCode::KeyD, Key::E => KeyCode::KeyE, Key::F => KeyCode::KeyF,
        Key::G => KeyCode::KeyG, Key::H => KeyCode::KeyH, Key::I => KeyCode::KeyI,
        Key::J => KeyCode::KeyJ, Key::K => KeyCode::KeyK, Key::L => KeyCode::KeyL,
        Key::M => KeyCode::KeyM, Key::N => KeyCode::KeyN, Key::O => KeyCode::KeyO,
        Key::P => KeyCode::KeyP, Key::Q => KeyCode::KeyQ, Key::R => KeyCode::KeyR,
        Key::S => KeyCode::KeyS, Key::T => KeyCode::KeyT, Key::U => KeyCode::KeyU,
        Key::V => KeyCode::KeyV, Key::W => KeyCode::KeyW, Key::X => KeyCode::KeyX,
        Key::Y => KeyCode::KeyY, Key::Z => KeyCode::KeyZ,
        // Digits
        Key::Num0 => KeyCode::Digit0, Key::Num1 => KeyCode::Digit1,
        Key::Num2 => KeyCode::Digit2, Key::Num3 => KeyCode::Digit3,
        Key::Num4 => KeyCode::Digit4, Key::Num5 => KeyCode::Digit5,
        Key::Num6 => KeyCode::Digit6, Key::Num7 => KeyCode::Digit7,
        Key::Num8 => KeyCode::Digit8, Key::Num9 => KeyCode::Digit9,
        // Navigation / editing
        Key::Escape    => KeyCode::Escape,
        Key::Tab       => KeyCode::Tab,
        Key::Backspace => KeyCode::Backspace,
        Key::Enter     => KeyCode::Enter,
        Key::Space     => KeyCode::Space,
        Key::Insert    => KeyCode::Insert,
        Key::Delete    => KeyCode::Delete,
        Key::Home      => KeyCode::Home,
        Key::End       => KeyCode::End,
        Key::PageUp    => KeyCode::PageUp,
        Key::PageDown  => KeyCode::PageDown,
        Key::ArrowUp    => KeyCode::ArrowUp,
        Key::ArrowDown  => KeyCode::ArrowDown,
        Key::ArrowLeft  => KeyCode::ArrowLeft,
        Key::ArrowRight => KeyCode::ArrowRight,
        // Punctuation
        Key::Comma        => KeyCode::Comma,
        Key::Period       => KeyCode::Period,
        Key::Slash        => KeyCode::Slash,
        Key::Backslash    => KeyCode::Backslash,
        Key::Minus        => KeyCode::Minus,
        Key::Equals       => KeyCode::Equal,
        Key::Plus         => KeyCode::Equal, // shifted: same physical key
        Key::Semicolon    => KeyCode::Semicolon,
        Key::Colon        => KeyCode::Semicolon,
        Key::Quote        => KeyCode::Quote,
        Key::OpenBracket  => KeyCode::BracketLeft,
        Key::CloseBracket => KeyCode::BracketRight,
        Key::Backtick     => KeyCode::Backquote,
        // F-keys (egui has no F5; iris likely doesn't need F13+ either)
        Key::F1 => KeyCode::F1, Key::F2 => KeyCode::F2,  Key::F3  => KeyCode::F3,
        Key::F4 => KeyCode::F4, Key::F6 => KeyCode::F6,  Key::F7  => KeyCode::F7,
        Key::F8 => KeyCode::F8, Key::F9 => KeyCode::F9,  Key::F10 => KeyCode::F10,
        // F11 is consumed by the GUI (fullscreen toggle); don't forward.
        Key::F12 => KeyCode::F12,
        _ => return None,
    })
}
