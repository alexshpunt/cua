# Opt-in retained WGC window observations

This Windows research-fork path is being qualified for LPT-695 / fork RFC #19. It is not an installed-runtime replacement or cross-platform capture certification.

## Request

`get_window_state` accepts `capture_backend: "default" | "wgc"`. Omission and `default` keep the existing capture path. Explicit WGC requires a trusted runtime session and a screenshot. `include_screenshot:false` without an output file is refused as `wgc_requires_screenshot`; it does not start a pixel worker. Other platforms do not advertise this option.

A WGC read uses the exact requested PID/window and runtime-injected session ID. An error never substitutes PrintWindow, another window or desktop pixels. The usual screenshot, snapshot and capture-ID publication remain the input boundary.

## Ownership and bounds

The existing Windows ToolState owns a manager. Each session/window worker initializes its own MTA, D3D device, capture item, two-frame pool, session and staging texture. Repeated reads reuse these resources, not a cached PNG. There are at most eight live or stopping workers and one queued request per worker. CPU content is limited to 64 MiB per frame. Idle workers close after five seconds.

Frame requests have a 1500ms budget including queue time and setup. A dropped awaiting request signals cancellation. Frame polling checks cancellation/session end at bounded intervals. A failed or abandoned read closes its worker. The existing scoped session-end hook stops only that session's workers and capture-mapping metadata. Manager destruction also stops owned workers.

The native D3D/WinRT calls themselves are OS calls: cancellation cannot forcibly interrupt a hung device/Map call or prove cleanup after process termination. The tool's existing outer timeout drops its awaiting request; the worker closes resources when control returns. Do not advertise a hard real-time cleanup guarantee for a wedged graphics driver.

## Geometry and freshness

Only a WGC content frame whose dimensions exactly match stable DWM extended-frame bounds may be published. The native worker removes the same one-pixel edge as the existing bitmap action domain. Texture padding is never treated as content or used to infer origin. A move/resize during a read, unknown bounds or mismatched extents refuses without input-grounding pixels.

Capture-bound input for a retained frame validates the current exact target and DWM action dimensions without recapturing through PrintWindow. Session/target/capture checks and one-use consumption stay in the common capture service. The existing bitmap-to-screen dispatch adds the current window origin once.

`capture_backend` response metadata reports the backend, resource reuse, measured setup/wait/readback boundaries, physical frame bounds and signed WGC presentation diagnostics. `paint_freshness` remains `unverified`: a later WGC stamp can be future-dated and does not prove changed application pixels. The output uses the existing one-final-PNG fast Bilinear path; no codec, license or dependency is changed.

## Qualification checkpoint

Geometry mapping was red before implementation (`8885729078dc`). Removing cancellation detection made the retained-resource owner test fail (`2807c393a1ee`); the mutation was restored. The complete Linux Windows-stub owner suite passed 118 tests, formatting and diff checks (`42a3e981eb0a`). These are hermetic contracts, not Windows native evidence.

Candidate `841dad7e11619bf7ecba3f98e1c20319cbeb7e52` passed Windows build [37929917740](https://github.com/alexshpunt/cua/actions/runs/37929917740): Windows owners, protocol/schema tests and release packaging. Its driver SHA256 is `cf45f42407b06da75c20d21e45da0c68093ccc43afeeaf460a5c6a286794d20d`.

Two disposable native fixtures passed 27 reads covering cold/warm image/full, five-second idle expiry, move/shrink/grow/cover, one capture-bound pixel hit with application pointer/click events, minimized/closed refusal and owner isolation. Some changed markers needed a second successful read; increasing stamps did not prove current pixels.

After adapter reload, production tools passed explicit image/full/image WGC reads, warm reuse, one grounded Save click and independent pointer/click events with counter1. UI-only inherited no pixels; invalid UI+WGC and unsupported native detail refused without fallback. The actual human TUI was readable. Adapter checks passed 378 unit tests, typecheck and 26 real-Pi tests.

Owned fixtures/profiles and peers were closed; task loading was removed and normal startup origins restored. Exact temporary candidate files were removed. External PowerShell process queries could not launch, so no independent Windows process-absence claim is made. No installed runtime/config, permissions or dependencies changed. This qualifies only the Windows opt-in fork contract, not a universal speed improvement or full cross-platform desktop matrix.

The tested executable source remains `841dad7e`; this qualification-note update changes no native behavior.
