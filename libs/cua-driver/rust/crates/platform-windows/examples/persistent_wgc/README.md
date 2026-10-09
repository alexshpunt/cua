# Retained WGC experiment

This is a Windows-only, read-only example inside CUA. It is not a Driver runtime, does not register MCP tools, and never authorizes input. The normal Driver still uses its existing guarded PrintWindow route. Related work: [fork issue16](https://github.com/alexshpunt/cua/issues/16).

The probe owns one exact PID/window for its whole life. Its first WGC request creates a hardware D3D device, capture item, two-buffer free-threaded pool and session. Later requests reuse them and the staging texture. An MTA apartment owns these resources. Frame events send a bounded wake notification only; CPU copying happens on the requesting thread.

The example lacks the Driver executable's DPI-awareness manifest. Its capture worker therefore enters and verifies Per-Monitor V2 awareness for physical pixels, then restores its old thread context at exit. This changes only the owned probe thread, not display settings or the installed runtime. The `ready` record reports `dpi: "per_monitor_v2"`.

Every WGC answer must have a `SystemRelativeTime` newer than the request's QPC start and the previous delivered frame. Queued old stamps are rejected. This ordering is a filter, not proof that the window's content changed: each experimental PNG needs an independent current-pixel witness. A request waits at most1500ms. Static or throttled windows may not produce a qualifying frame; a timeout is a failed observation, not proof that the last pixels are current.

`ContentSize` defines the valid region of the texture. A size change closes the checked-out frame, recreates the pool and discards staging storage before trying again. Three unsettled changes stop the request. CPU copying checks dimensions and row pitch and is limited to64MiB. Every checked-out frame is closed; every mapped texture is unmapped, including error paths.

The probe rejects a dead process, missing/mismatched HWND, changed process creation time, minimized target or closed capture item. The WGC item remains bound to its original window. These checks are not proof of every possible same-process HWND-reuse race; live identity qualification is still required. No desktop-region substitution or input geometry is invented.

EOF, any request failure, five seconds without a command,128requests or300seconds total closes the retained session/pool and exits. Shutdown is local to this executable. It cannot keep capture alive in the installed Driver. The input queue holds one command and each line is limited to4096bytes. Only a simple output basename is accepted; PNG files are written to the explicitly selected existing directory outside the measured native interval. Existing files are not overwritten.

## Build and run

```sh
cargo test --locked -p platform-windows --example persistent-wgc-probe
cargo build --locked -p platform-windows --example persistent-wgc-probe --release --target x86_64-pc-windows-msvc
```

On Windows, run the release example with the PID/HWND of a newly owned disposable fixture and an existing private output directory:

```text
persistent-wgc-probe.exe PID HWND OWNED_OUTPUT_DIRECTORY
```

The program emits a small `ready` JSON record. Send newline-delimited commands:

```json
{"id":1,"route":"printwindow","output":"baseline-001"}
{"id":2,"route":"wgc","output":"candidate-001"}
{"id":3,"route":"close","output":"unused"}
```

`printwindow` calls CUA's unchanged guarded overview helper, including its existing fallbacks; it is not a forced pure-PrintWindow measurement. `wgc` calls the retained session directly. Both use the same existing Bilinear/one-final-PNG helper and500px cap. They may have different capture extents. Save and review the actual pixels and geometry rather than assuming interchangeable input coordinates.

An isolated `wgc_next_delivery` route tests a stricter local queue boundary. It closes at most eight queued frames, drains old wake notifications and samples a local QPC barrier. Then it waits for a FrameArrived callback delivered after that barrier before selecting a frame. It retains the same raw-stamp and previous-frame filters, 1500ms deadline and resource limits. Resize retries establish a new local barrier. Callback work is only a QPC read and bounded notification, not a GPU copy. There is no pacing sleep or extra wait for a future presentation stamp.

The route reports `persistent_wgc_next_delivery`, `drained_before_wait`, `arrival_barrier_100ns` and `arrival_observed_100ns`. These prove local delivery ordering only: a later arrival can still contain unchanged content. The original `wgc` route remains for side-by-side comparison. New transition harness measurements count old PNGs and include the wall time to a verified current revision; no speed or freshness result is claimed before this variant is built and measured.

The `stats` record separates first WGC setup, frame wait, CPU readback and image processing. The whole native interval excludes PNG file writing and stdio transport. Local `dequeued_100ns` and `copy_completed_100ns` measure delivery-to-copy time; dequeue is not the original FrameArrived callback. `frame_timestamp_100ns` stays unchanged. Its signed `reported_frame_age_at_copy_ms` may be negative and is diagnostic, not actual pixel age. Local clocks must still be ordered.

Vsync windows can report a future presentation stamp, as [Vypr's measured implementation describes](https://github.com/Amzi-01/Vypr/releases/tag/v0.5.0). Our probe observed a stamp1.0708ms after copy completion. It does not reject solely for that, clamp the reported age, or wait artificially for presentation time. PrintWindow has no equivalent frame stamp here. `processing_ms` combines channel conversion, resize and PNG; it is not an exclusive PNG timer.

The fork Windows workflow builds this example only when its source is present. It packages it separately under `probe/`, with `probe-hash.json`, not among the three Driver runtime executables. Match the source commit and hash before running it. Never install this example as the selected runtime.

## Evidence needed

Before recommending retained capture, compare alternating cold/warm requests on one changing-content fixture, check every new color in the actual saved PNG, and separately exercise covered/moved/resized/minimized/closed targets and bounded idle/error cleanup. Inspect the pictures yourself. Missing checks remain missing; a successful capture or a fast receipt alone does not qualify freshness, input mapping or adoption.

The initial three hermetic policy tests and 107 Linux platform tests passed. Live checks corrected successful-null HRESULT0 handling and missing probe-thread DPI awareness, with Windows binding/restoration tests. A diagnostic build confirmed a WGC stamp1.0708ms ahead of copy completion. Five policy tests now cover the user's approved softer handling: raw signed metadata remains separate from strictly ordered local clocks.

Exact source `816f8354e099c447af4dae53e9a6f1a3c7c35b6e` passed Windows build37912695218. Ten alternating warm pairs on a prepared, unique-color fixture passed, with native medians46.56ms guarded PrintWindow versus25.70ms WGC. Explicit/idle/error/closed-target process-exit checks and all owned cleanup passed. Installed runtime/config stayed unchanged. No dependency version or license changes were needed.

This result is prepared-surface capture, not reaction latency. Preparation independently checks current pixels through the unchanged Driver and is excluded from the timed capture. An earlier transition trial returned an old revision despite an increasing WGC stamp. Covered/moved/resized/minimized targets and production input mapping remain unqualified. See the [harness report and redacted measurements](https://github.com/alexshpunt/pi-agent-computer-use/blob/experiment/lpt-673-persistent-wgc/docs/persistent-wgc-experiment.md). Keep the native PR draft; these timings do not authorize adoption.
