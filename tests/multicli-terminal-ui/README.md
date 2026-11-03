# Native terminal UI pressure fixture

This is an explicit Windows test entry, never a product startup action. It uses
the production `TerminalManager`, ConPTY, Tauri IPC, `TerminalPane` and xterm.
Eight local synthetic helpers emit 100 KiB/s each for 60 seconds, with 40 input
ACK samples in the visible pane. Seven hidden panes keep draining. Twenty rounds
run in one native process and reuse the same manager and WebView.

No CLI accounts, prompts, original histories or product profiles are used. Choose
a fresh absolute output directory outside the repository. Keep the window visible
and avoid running builds during the measurement.

From the app checkout, in PowerShell, after installing the existing locked
dependencies and configuring the project's Rust/native build environment:

```powershell
$env:RUSTUP_TOOLCHAIN = '1.99.1'
$benchOutput = Join-Path $env:TEMP ('multicli-terminal-ui-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $benchOutput | Out-Null
cargo build --manifest-path src-tauri/Cargo.toml --example multicli_terminal_ui_bench
rustc tests/multicli-terminal-ui/flood-echo.rs -o (Join-Path $benchOutput 'flood-echo.exe')
node node_modules/typescript/lib/tsc.js --project tests/multicli-terminal-ui/bench-tsconfig.json
node node_modules/vite/bin/vite.js --config tests/multicli-terminal-ui/bench-vite.config.mjs
```

Keep Vite on loopback port 1437. In another PowerShell, use the same absolute
`$benchOutput` value:

```powershell
& src-tauri/target/debug/examples/multicli_terminal_ui_bench.exe `
  --root $benchOutput --helper (Join-Path $benchOutput 'flood-echo.exe') `
  --rounds 20 --seconds 60 --samples 40
python tests/multicli-terminal-ui/verify_report.py (Join-Path $benchOutput 'bench-report.json')
```

An initial smoke can use `--rounds 1 --seconds 5 --samples 4` with a separate
output directory. It does not satisfy the full validator. The app saves a report
after every round and exits after completion. Do not edit imported sources or
the fixture while running; Vite reload would invalidate a same-process run.

Timing starts at a monotonic scheduled input deadline aligned to the helpers'
common start barrier. It includes delayed JS dispatch, native IPC, helper ACK,
production xterm parsing and two animation-frame callbacks. `invokeLatencyMs`
and `dispatchDelayMs` are retained separately. This does not inject OS keyboard
events or measure physical display scanout. Native IME, clipboard, tray and mouse
acceptance are separate manual checks.

The report contains source bytes/duration for every helper, samples and failures,
native and WebView-tree handles/working sets, identity-aware child exit checks,
remaining managed terminals and durable unfinalized Runs. Review the entire
handle curve for continuing growth; the validator prints the curve rather than
inventing a permissible leak allowance. Working set includes WebView/GPU caching
and is distinct from the 4 MiB terminal byte-ring bound. `bytes` read through
ConPTY includes console echo/control sequences; use `sourceFlood` for the paced
helper's exact byte count.

Process ancestry also compares native creation timestamps. A process predating
its currently numbered parent is excluded as a reused-PPID link; unknown ancestry
is reported rather than silently counted as an owned descendant.

This fixture was relocated from the supervisor's isolated S04 harness with only
import/config paths changed. Raw reports, executables, profiles, databases and
screenshots remain outside Git.
