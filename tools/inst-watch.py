#!/usr/bin/env python3
"""
Watch an IRIX install console log and emit a structured event whenever the
guest is waiting on input (output has been quiescent for N seconds).

Designed to be run as a Monitor-style background process while driving the
IRIX 6.5.22 install through iris-ci. Each event line is self-contained and
classifies the prompt so the driver can react without re-reading the log.

Usage:
    tools/inst-watch.py [--log PATH] [--quiet-secs N] [--json]

Defaults:
    --log irix-install-console.log
    --quiet-secs 4
    human-readable output (use --json for one-line NDJSON per event)

Event format (human):
    === STALL kind=<type> @ <bytes>B
    <relevant tail line(s)>
    === /STALL

Event format (--json):
    {"kind": "...", "bytes": N, "tail": [...lines...], "hint": "..."}

Classification:
  cd_swap         -- "Please insert the X CD" / "Insert ... press"
  yn_confirm      -- ends with (y/n) or (yes/no)
  numbered_choice -- "Please enter a choice [N]:" / "[1]:"
  press_enter     -- "press <Enter>" / "press any key"
  install_software_from -- "Install software from: [...]" (the from-loop prompt)
  inst_ready      -- last line is "Inst>"
  sash_ready      -- last line is "sash:" or ">>"
  fx_prompt       -- "fx>" or "fx/...>" or "fx: "
  prom_option     -- "Option?"
  mkfs_confirm    -- "Make new file system"
  block_size      -- "Block size in bytes"
  restart_confirm -- "Restart? {" or "Restart the system"
  license_pager   -- pager-style "--More--" or just hung after license text
  conflict_err    -- "Conflicts must be resolved"
  error           -- "ERROR:"
  panic           -- "PANIC" / "Exception" / "UTLB Miss"
  switch_dist     -- "Do you really want to switch distributions"
  miniroot_fix    -- the sash "Enter 'c' to continue / 'f' to fix" prompt
  generic_prompt  -- last line ends with :/?/>/]/) but no specific match
  alert           -- matched alert keyword but tail does not end on prompt char
                     (advisory; safe to ignore if not actionable)
"""

import argparse
import json
import re
import sys
import time
from pathlib import Path


CLASSIFIERS = [
    # (kind, pattern, hint)
    # Order matters: most specific first.
    ("panic",            re.compile(r"\b(PANIC|UTLB Miss|Exception PC|Unexpected exception)"),     None),
    ("error",            re.compile(r"\bERROR:"),                                                  None),
    ("conflict_err",     re.compile(r"Conflicts must be resolved"),                                 "send 'conflicts' then resolve via tools/inst-resolve.py"),
    ("switch_dist",      re.compile(r"Do you really want to switch distributions\?\s*\(y/n\)"),    "respond y to switch + lose selections, n to keep current"),
    ("mkfs_confirm",     re.compile(r"Make new file system on .*\[yes/no"),                        "respond yes for a fresh disk"),
    ("block_size",       re.compile(r"Block size in bytes"),                                       "respond 4096 for >=4GB disks"),
    ("cd_swap",          re.compile(r'(?:Please )?[Ii]nsert the ["\'][^"\']+["\'] CD'),            "cdrom-eject 4 until the named CD is mounted, then press enter"),
    ("cd_swap",          re.compile(r"Insert .* CD-ROM.*press"),                                   "cycle CD changer to required disc, then press enter"),
    ("restart_confirm",  re.compile(r"Restart\?\s*\{|Restart the system"),                          "respond y to reboot into installed system"),
    ("miniroot_fix",     re.compile(r"Enter your selection and press ENTER \(c, f, or a\)"),       "respond f to reset miniroot install state"),
    ("press_enter",      re.compile(r"press (?:<Enter>|<enter>|any key)"),                         "send empty string (Enter)"),
    ("license_pager",    re.compile(r"--More--|\bq to quit", re.IGNORECASE),                       "send q to dismiss"),
    ("yn_confirm",       re.compile(r"\((?:y/n|yes/no|y or n)\)\s*$", re.IGNORECASE),              "respond y or n"),
    ("numbered_choice",  re.compile(r"Please enter a choice \[(\d+)\]"),                           "respond with menu number"),
    ("numbered_choice",  re.compile(r"\[\d+\]:\s*$"),                                              "respond with menu number or enter for default"),
    ("install_software_from", re.compile(r"Install software from:\s*\[[^\]]+\]\s*$"),              "send /CDROM/dist[/subpath] OR 'done' to exit from-loop"),
    ("inst_ready",       re.compile(r"^\s*Inst>\s*$", re.MULTILINE),                               "send next inst command"),
    ("sash_ready",       re.compile(r"^\s*sash:\s*$|^\s*>>\s*$", re.MULTILINE),                    "send sash command (boot ...) or 'exit'"),
    ("fx_prompt",        re.compile(r"^\s*fx[>/][\w/]*>\s*$|^\s*fx:\s", re.MULTILINE),             "send fx command"),
    ("prom_option",      re.compile(r"^\s*Option\?\s*$", re.MULTILINE),                            "respond with menu number 1-5"),
]

PROMPT_CHARS = set(":?>])")


PROGRESS_LINE = re.compile(r"\b\d{1,3}%(?:\s|$)|^Installing\s+(?:new|the)|^Reading\s+product")


def classify(tail_text: str):
    """Return (kind, hint) or (None, None) if no match."""
    # If the LAST non-empty line is a progress/installing/reading line, the
    # install is actively making progress — the classifier matchers below
    # would otherwise fire on stale "Please insert" text still in the tail
    # window from before we swapped the CD. Skip in that case.
    lines = [ln for ln in tail_text.split("\n") if ln.strip()]
    if lines and PROGRESS_LINE.search(lines[-1]):
        return None, None
    for kind, pat, hint in CLASSIFIERS:
        if pat.search(tail_text):
            return kind, hint
    # Generic fallback: tail ends with a prompt character
    stripped = tail_text.rstrip()
    if stripped and stripped[-1] in PROMPT_CHARS:
        return "generic_prompt", None
    # Last-resort: did we see any of the alert keywords?
    if re.search(r"failed|aborted|cancelled|hang", tail_text, re.IGNORECASE):
        return "alert", "advisory — check tail"
    return None, None


def tail_lines(path: Path, max_bytes: int = 1200, n_lines: int = 6) -> list[str]:
    """Return the last n_lines non-empty lines from up to max_bytes of file tail.

    Strips \\r so progress-only updates collapse to their final state. Trailing
    whitespace is kept so prompt-detection sees the literal prompt characters.
    """
    try:
        size = path.stat().st_size
    except FileNotFoundError:
        return []
    with path.open("rb") as f:
        f.seek(max(0, size - max_bytes))
        raw = f.read()
    text = raw.decode("utf-8", errors="replace").replace("\r", "")
    lines = [ln for ln in text.split("\n") if ln.strip()]
    return lines[-n_lines:]


def emit(out, ev: dict, as_json: bool):
    if as_json:
        out.write(json.dumps(ev, ensure_ascii=False))
        out.write("\n")
    else:
        hint = f" hint={ev['hint']!r}" if ev.get("hint") else ""
        out.write(f"=== STALL kind={ev['kind']} @ {ev['bytes']}B{hint}\n")
        for ln in ev["tail"]:
            out.write(ln + "\n")
        out.write("=== /STALL\n")
    out.flush()


def watch(path: Path, quiet_secs: float, as_json: bool, poll: float = 1.0):
    out = sys.stdout
    prev_size = path.stat().st_size if path.exists() else 0
    last_emitted_size = -1
    quiet_for = 0.0
    fired = False
    while True:
        time.sleep(poll)
        cur_size = path.stat().st_size if path.exists() else 0
        if cur_size != prev_size:
            prev_size = cur_size
            quiet_for = 0.0
            fired = False
            continue
        quiet_for += poll
        if fired or quiet_for < quiet_secs or cur_size == last_emitted_size:
            continue
        # Look at last lines and classify
        lines = tail_lines(path)
        if not lines:
            continue
        # Join with newlines to give classifiers anchoring (^/$ work per-line in MULTILINE)
        tail_text = "\n".join(lines) + "\n"
        kind, hint = classify(tail_text)
        if kind is None:
            # Quiescent but doesn't look like a prompt — skip (probably mid-progress).
            continue
        fired = True
        last_emitted_size = cur_size
        emit(out, {
            "kind": kind,
            "bytes": cur_size,
            "tail": lines,
            "hint": hint,
        }, as_json)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--log", default="irix-install-console.log", help="install console log to tail (default: irix-install-console.log)")
    ap.add_argument("--quiet-secs", type=float, default=4.0, help="seconds of no new output before emitting a stall event (default: 4)")
    ap.add_argument("--json", action="store_true", help="emit one NDJSON event per stall instead of human-readable blocks")
    ap.add_argument("--poll", type=float, default=1.0, help="poll interval in seconds (default: 1.0)")
    args = ap.parse_args()
    watch(Path(args.log), args.quiet_secs, args.json, args.poll)


if __name__ == "__main__":
    main()
