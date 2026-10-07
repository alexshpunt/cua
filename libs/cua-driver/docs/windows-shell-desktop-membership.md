# Windows shell desktop membership

This fork adds a read-only `virtual_desktop` field to Windows `list_windows` records and `get_window_state` results. It uses the documented `IVirtualDesktopManager` interface. The field is optional in the portable contract; other platforms keep their existing workspace metadata.

```json
{
  "virtual_desktop": {
    "desktop_id": "00112233-4455-6677-8899-aabbccddeeff",
    "on_current_desktop": false,
    "desktop_id_error": null,
    "on_current_desktop_error": null,
    "sampled_at_unix_ms": 1791380000000
  }
}
```

`desktop_id` is the shell desktop hosting this window. `on_current_desktop` is the result of the separate current-desktop query. A switch of the active desktop need not change the GUID. Moving a window can change the GUID without changing its HWND/process lifetime.

Each value is either known with a null error, or null with its query error. Error records contain a typed `code` and a signed 32-bit `hresult` when Windows supplied one. COM initialization and manager failures affect both queries. One method's failure does not erase the other method's successful result. A zero or malformed GUID becomes `invalid_desktop_id`, never a guessed current desktop.

The timestamp marks completion of the membership queries, not capture time or an atomic desktop snapshot. Membership can change immediately after sampling. The query context lives on one COM thread and is dropped there; it does not keep a process-wide desktop cache.

Reads do not switch desktops, activate windows or move them. They validate the exact HWND/process pair before and after the queries. They do not confer input permission, prove fresh pixels or establish that CUA can capture or operate a hidden window. Window lifetime, monitor geometry, shell membership and observation freshness remain separate.

This is not a complete desktop inventory. The public interface has no method to list every desktop, including empty ones. A caller can group only the desktop GUIDs seen through permitted windows and must describe that list as incomplete. `EnumDesktopsW` enumerates a different Win32 window-station concept. The existing `virtualdesk.rs` refers to the monitor coordinate union, not Task View desktops.

No undocumented shell interface, registry inventory, other user session, desktop lifecycle, new rights or telemetry is introduced. Installed runtime selection and upstream release qualification remain separate from this additive fork change.

## Evidence

The portable contract tests own GUID validation, independent errors and preserving metadata through typed window results. The Windows fork build compiles the real COM producer and runs contract, core, Windows and protocol/schema owner tests. Live two-desktop qualification is pending; no cross-desktop capture/input or full desktop-matrix pass is claimed.

[Proposal and fork-owner decision](https://github.com/alexshpunt/cua/issues/8)

[Documented Windows interface](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager)
