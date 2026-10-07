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

Local deterministic owners currently pass: 61 contract, 842 core and 101 Windows-crate tests. New topology and native authority tests failed before implementation and then passed. Candidate Windows compilation and two-monitor live verification are still pending. No installed runtime, monitor setup or permission settings were changed. Temporary candidate use and installation require separate owner approval.
