# Windows held paths in this fork

This fork adds optional `via` points to Windows `drag`. Calls without `via` keep their existing straight-drag routes. Other platforms and generated SDK bindings are not expanded. The fork decision is [RFC #1](https://github.com/alexshpunt/cua/issues/1).

## Request

Read `get_window_state` for the exact window first. Every coordinate is local to its returned screenshot. For example:

```json
{
  "pid": 1234,
  "window_id": 5678,
  "from_x": 100,
  "from_y": 100,
  "via": [{"x": 130, "y": 150}, {"x": 170, "y": 160}],
  "to_x": 200,
  "to_y": 100,
  "duration_ms": 800,
  "delivery_mode": "foreground"
}
```

The example IDs and points are placeholders, not a runnable target.

- `via` has 1–254 finite, non-negative points. Endpoints make 3–256 points in total.
- The left button is pressed once at the start and released after the last point.
- All points use the same saved screenshot scale. Window movement changes their screen origin, not their local coordinates. Points outside current window/display bounds refuse or stop.
- `duration_ms` is 0–10000, default 500. It spaces moves over the requested duration; it is not a real-time guarantee.
- Explicit `pid` and `window_id` are required. Desktop scope, modifiers, explicit `steps`, right/middle buttons and zoom coordinates are not supported for this path.
- Background delivery refuses before path input. Foreground is an explicit escalation that uses the existing Windows activation helper, then verifies the exact target before every path event. It moves the real pointer.

## Receipts and failure

The public action result stays in the driver's closed schema. `delivery.delivered_count` is the number of completed point moves, including the start. `summary` reports total points, press/release receipts and requested/measured duration. A complete native delivery remains `unverifiable`: read the app to establish whether it drew the intended stroke.

A failed path reports its actual prefix. A release failure must not claim release confirmation. A worker panic leaves prefix/release unconfirmed. Never automatically replay any of these cases.

The native worker keeps physical-input admission until release/cleanup finishes. Dropping its caller sets a cancellation flag; the worker checks it during waits and before subsequent events. A foreground change, lost/hidden/minimized target or out-of-bounds point stops further movement and attempts button-up. Restoration never takes foreground away from a window that the user chose while the path was running.

Use the existing upstream EOF-aware receiver with child-local `CUA_DRIVER_MCP_ENVELOPES=1`. Check its `ai.cua.driver.envelopes` version 1 capability at MCP initialization. It accepts ordinary tool calls and reads EOF while a path is active, drops the invocation and signals native cleanup. The default serial MCP loop cannot notice EOF until the active call finishes; do not qualify it for held paths. Legacy cancellation notifications are ignored in both modes. A bare client timeout is not a release boundary, and forcibly killing the process cannot run its cleanup.

## Verification boundary

Hermetic tests own validation, event order, completed prefix, release and native-worker admission. The manual Windows workflow runs those owner crates and builds exact source bytes. Opt-in desktop checks must establish native event receipt, cancellation/release and existing-tool compatibility. Final acceptance still requires a new Paint picture made with its brush; a build or synthetic canvas alone is not acceptance.

The candidate is unsigned. No certificate trust, elevation, OS input policy or installed runtime is changed to make it pass. This bounded Windows fork work is not upstream cross-platform E2E certification.
