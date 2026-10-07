# Selected Windows displays in the research fork

This is an additive Windows-only fork feature. It is not an upstream release or a cross-platform support claim. [Fork decision](https://github.com/alexshpunt/cua/issues/4).

## Public reads

`list_displays({session?})` returns `platform`, `topology_id` and `displays`. Each display has:

- `display_id`: the native GDI device name, not a primary alias or guessed physical identity;
- `monitor_handle`: the native HMONITOR as a string;
- `monitor_device_paths`: native monitor interface names, possibly several for a mirrored logical monitor;
- `bounds` and `work_area`: `{x,y,width,height}` in host-global physical pixels;
- `primary`: the native monitor flag;
- `scale_factor`: successful `GetScaleFactorForMonitor` evidence, or null;
- `dpi`: null, not DPI inferred from scale or another window.

The ID must be used with its topology token. GDI names and monitor handles alone are not durable across device changes. Enumeration order does not affect the token; identity, monitor interface names, geometry, work area, primary status and scale do.

`get_display_state({display_id,topology_id,session?,max_image_dimension?})` reads only that native monitor. Zero or omitted image cap keeps native resolution. The PNG uses display-local pixels with origin `(0,0)`; `display.bounds` stays host-global and may start at a negative coordinate. Original and returned screenshot dimensions are separate.

There is no file-output argument, input-authorizing `capture_id`, desktop-scale mutation, window tree or selected-display input. Existing `get_desktop_state` and primary desktop action routes stay unchanged.

## Refusals and permission

The read checks topology before and after GDI capture. A missing target returns `display_unavailable`; mismatched topology returns `display_topology_changed`. Invalid or ambiguous native topology returns `display_topology_invalid`. Other native failures return `display_read_failed`. Errors do not include pixels and never retry against primary.

Both tools use desktop capture scope and private-observation authorization. Selected-display protected scope is attested by native discovery and bound to the exact display/topology. Native capture keeps the existing CUA agent-overlay exclusion. These pixels are read geometry, not an input grant.

## Native evidence and limits

Physical geometry is read and captured on a scoped PMv2-aware worker thread. The previous thread awareness is restored. Scale comes from [GetScaleFactorForMonitor](https://learn.microsoft.com/en-us/windows/win32/api/shellscalingapi/nf-shellscalingapi-getscalefactorformonitor) only on success. DPI remains unknown: Microsoft warns that [GetDpiForMonitor](https://learn.microsoft.com/en-us/windows/win32/api/shellscalingapi/nf-shellscalingapi-getdpiformonitor) should not be called by a per-monitor-aware thread; `GetDpiForWindow` would describe a window instead.

The token describes observed topology, not an OS transaction or permanent physical-monitor lifetime. Capture can reflect ordinary live paint changes. Re-enumerate and take a fresh overview after any topology refusal; never reinterpret an old local crop on a changed target.

## Qualification checkpoint

The unchanged qualified source `3b3305044b7b327310615aa99d3307948a63061c` passed [Windows build 37595775262](https://github.com/alexshpunt/cua/actions/runs/37595775262), including native owner tests, protocol/schema checks and the release bundle. The base branch also contains a later documentation-only commit `4c54887bb7d0ffc9c0dcc8abe5b6423a67b59ca9`.

Exact source `38f50121c9cc15938a18330f4c3334a27f898e83` passed [Windows build 37602770786](https://github.com/alexshpunt/cua/actions/runs/37602770786): 829 core and 287 Windows tests (one ignored), two protocol and four schema checks, release build and bundle. Local deterministic owners passed 61 contract, 842 core and 102 Windows-crate tests. The live-schema startup assertion was reproduced red/green in a public tool-definition regression test. Earlier failed candidates are not acceptance evidence.

The checked unsigned Driver 0.34.0 binary has SHA-256 `1dbd499cfd43b98f31dc3fb51c7a001dc72bd116db235c304c0486b32df8a24d`. Exact bundle hashes, version, 62 native tools, envelope support and inherited drag/key-hold capabilities were checked before execution.

Approved opt-in Windows checks on 2026-10-07 matched separate OS reports for both connected logical monitors:

| Logical monitor | Primary | Host-global physical bounds | Work area | Scale | DPI |
| --- | --- | --- | --- | --- | --- |
| `\\.\DISPLAY1` | No | 2560,0, 3840×2560 | 2560,0, 3840×2488 | 1.5 | Unknown |
| `\\.\DISPLAY2` | Yes | 0,0, 2560×1440 | 0,0, 2560×1368 | 1.5 | Unknown |

Two isolated Chrome fixture profiles were placed through existing CUA window setup, not a new relocation feature. Each exact selected capture matched an independent native window pixel sample: blue `[37,73,245]`, green `[96,201,115]`. Native color processing changed the CSS RGB values, so CSS alone was not the pixel oracle. Missing ID and wrong topology returned exact refusal codes without images. Legacy primary and window captures still worked. Chrome background input refused explicitly; fresh observation and explicit foreground navigation/readback worked.

The adapter's real production tools were reloaded and checked on both monitors, including fresh local crops, no input authority, cross-source and consumed-ID refusal, private application-boundary image/source matching and the real TUI. Negative origins, mixed scale, topology changes, Safe/revocation and cancellation were checked by deterministic contracts; no live unplug or OS setup change was made. A display capture sees occluding windows, unlike native background-window capture.

The owner separately approved temporary testing, then keeping the exact artifact in a new folder. Older installed builds were preserved. The permanent-folder artifact and native fixture checks passed again. The first production run's owned fixture windows and profiles were cleaned up; each opt-in runner also checks its own cleanup.

This evidence is a focused Windows fork qualification, not a full cross-platform desktop matrix, upstream release or confirmed final model-wire payload. The qualification-doc update after `38f50121` changes no executable bytes.
