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

The `stats` record separates first WGC setup, frame wait, CPU readback and image processing. The whole native interval excludes PNG file writing and stdio transport. Local `dequeued_100ns` and `copy_completed_100ns` measure delivery-to-copy time; dequeue is not the original FrameArrived callback. `frame_timestamp_100ns` stays unchanged. Its signed `reported_frame_age_at_copy_ms` may be negative and is diagnostic, not actual pixel age. Local clocks must still be ordered.

Vsync windows can report a future presentation stamp, as [Vypr's measured implementation describes](https://github.com/Amzi-01/Vypr/releases/tag/v0.5.0). Our probe observed a stamp1.0708ms after copy completion. It does not reject solely for that, clamp the reported age, or wait artificially for presentation time. PrintWindow has no equivalent frame stamp here. `processing_ms` combines channel conversion, resize and PNG; it is not an exclusive PNG timer.

The fork Windows workflow builds this example only when its source is present. It packages it separately under `probe/`, with `probe-hash.json`, not among the three Driver runtime executables. Match the source commit and hash before running it. Never install this example as the selected runtime.

## Evidence needed

Before recommending retained capture, compare alternating cold/warm requests on one changing-content fixture, check every new color in the actual saved PNG, and separately exercise covered/moved/resized/minimized/closed targets and bounded idle/error cleanup. Inspect the pictures yourself. Missing checks remain missing; a successful capture or a fast receipt alone does not qualify freshness, input mapping or adoption.

The initial three hermetic policy tests and107 Linux platform tests passed. First Windows build passed, then live requests exposed incorrect handling of an empty queue (windows-core0.58 reports HRESULT0). Corrected empty-queue policy and Windows binding tests passed in build37900786907. That live trial then exposed mismatched DPI geometry and failed a current-color check, so it produced no valid warm comparison. The DPI correction and its Windows restoration test passed in build37902932487. Further live trials exposed a future WGC presentation stamp; diagnostic build37910189026 confirmed the1.0708ms lead. The user's approved softer handling keeps the raw signed diagnostic and validates local timing separately. Its Windows/live evidence is pending. No speed improvement is claimed yet. No dependency version or license changes are needed; this example uses the CUA workspace's existing libraries.
