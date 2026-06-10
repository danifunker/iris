# App Store Listing — IRIS

Copy/paste fields for App Store Connect. Character limits noted in parentheses.

## Name (≤30)
IRIS — SGI Indy Emulator

## Subtitle (≤30)
Boot IRIX on a virtual Indy

## Promotional Text (≤170)
A faithful SGI Indy workstation emulator that boots real IRIX. Bring your own
original IRIX media and rediscover late-'90s Unix on a modern Mac.

## Keywords (≤100, comma-separated, no spaces)
SGI,Indy,IRIX,MIPS,R4400,emulator,workstation,Unix,retro,vintage,REX3,emulation

## Description (≤4000)
IRIS is an emulator of the SGI Indy — the iconic blue MIPS workstation from the
mid-1990s. It recreates enough of the Indy's hardware to boot IRIX, SGI's Unix,
all the way to a graphical desktop with a shell, networking, and X11.

IMPORTANT — IRIX MEDIA REQUIRED
IRIS does not include IRIX or any SGI software. To use IRIS you must supply your
own original SGI IRIX installation media (IRIX 6.5 or 5.3). IRIX is not included
with IRIS and is not available from this developer. IRIS is an independent
project and is not affiliated with, authorized by, or endorsed by Silicon
Graphics.

WHAT IT EMULATES
• MIPS R4400 CPU (SGI Indy, IP22)
• REX3 / Newport graphics with a live framebuffer
• HAL2 audio
• SEEQ 8003 Ethernet with built-in NAT networking
• WD33C93 SCSI for hard-disk and CD-ROM images
• IndyCam (VINO) video input via your Mac's camera
• PS/2 keyboard and mouse, serial console

FEATURES
• Boots IRIX 6.5 and 5.3 to a usable graphical system
• Built-in machine manager — create, configure, and switch between virtual Indys
• Attach raw, IMG, or CHD disk images and ISO/CHD CD-ROMs
• Copy-on-write overlays so your original disk images stay pristine
• Save and restore machine snapshots
• Adjustable RAM, display scaling, and more

PERFORMANCE
IRIS is intentionally not cycle-accurate — it favors speed so IRIX feels
responsive on a modern Mac. A built-in JIT accelerates graphics; the emulator
also runs in a pure-interpreter mode.

WHO IT'S FOR
Retrocomputing enthusiasts, former SGI users, and anyone curious about the
workstation Unix that powered 1990s film, science, and 3D graphics.

You bring the IRIX media; IRIS brings the Indy.

## URLs
- Support URL:   https://github.com/danifunker/iris
- Marketing URL: https://github.com/danifunker/iris

## Category
- Primary: Developer Tools
- Secondary: Utilities

## Age Rating
4+ (no objectionable content)

---

## App Review Information — Notes (private; paste into the "Notes" field)
This app emulates an SGI Indy (MIPS R4400) workstation.

1) IRIX media: IRIS does not include or distribute any SGI/IRIX software. Users
   supply their own legally obtained original IRIX installation media. The app
   is fully functional and launches to its configuration UI WITHOUT any IRIX
   media present — you do not need IRIX to verify that the app runs.

2) JIT entitlement: the build uses the Cranelift JIT to emulate the MIPS CPU and
   requires com.apple.security.cs.allow-jit (declared in the app's
   entitlements). Without JIT the app falls back to an interpreter.

3) Camera: com.apple.security.device.camera is used only to provide the IndyCam
   (VINO) video-input device when the user enables it; NSCameraUsageDescription
   explains this to the user.

4) Networking: the client/server network entitlements drive the emulated
   SEEQ 8003 Ethernet with userspace NAT. The app itself contacts no external
   services.

## What's New (for updates)
Initial App Store release of IRIS, the SGI Indy emulator.
