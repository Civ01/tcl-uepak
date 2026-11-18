# Driving Total Commander under Wine (Linux)

Used to test the plugin inside a real TC (11.58 portable, evaluation copy) on a
virtual X display. Not part of the build.

- `wkeys/`: small Windows helper, run inside Wine. Sends keys by Windows
  virtual-key code (`down`, `f5`, `alt+shift+f9`, `text:...`, `wait:MS`,
  `focus:TITLE`) and dumps top-level windows with their controls (`dump`),
  which reads dialog texts and checkbox states without OCR. Keystrokes sent
  with `xdotool` arrive garbled under Wine on Xvfb; mouse clicks are fine.
  Build: `cargo build --release --target x86_64-pc-windows-gnu`.
- `t`: shell helper (`t start`, `t k KEYS...`, `t dump`, `t shot NAME`,
  `t log N`). `t start` also answers the evaluation nag screen.
- `start.sh`, `wincmd.ini`: start TC with its own ini (plugin registered for
  `pak`, hidden files shown). Paths point at `/home/claude/tctest`; adjust.

Setup: `Xvfb :50 -screen 0 1280x900x24 &`; portable TC unpacked to
`tctest/tc/portable`; plugin + `uepak.ini` (with `DebugLog=1`) + `deps\` in
`tctest/plugins/uepak`. The plugin log shows every call TC makes.

Note: Ubuntu's Wine 9.0 wineserver crashes when `TMPDIR` is set; every
command here unsets it.
