# Capture-bound straight drag on Windows

This fork adds optional `capture_id` to the live Windows `drag` tool. It is not a portable drag feature and does not change calls without `capture_id`.

1. Capture the exact window with `get_window_state`.
2. Use both endpoints in that returned screenshot's pixels.
3. Call `drag` with its `capture_id`, exact `pid` and `window_id`.
4. Reobserve after the attempt. Never remove the binding to retry a refusal.

The driver checks the live window owner and captured native dimensions before input. It validates both endpoints together, maps each through the capture transform once, then consumes the capture once. Window-to-screen conversion adds the current window origin; callers must not add monitor offsets or DPI scaling.

A changed native frame returns `capture_frame_mismatch`. An endpoint outside the encoded screenshot returns `capture_coordinate_invalid`. These refusals do not consume the capture or dispatch input. Wrong targets, expired captures and retired sessions use the existing capture admission errors.

`capture_id` requires a straight window drag. Desktop scope, `via`, zoom coordinates and an omitted exact target are refused. The existing held-path and desktop routes are unchanged. Other platforms do not advertise this Windows-only field.

Admission checks dimensions, not whether application content has changed. It does not guarantee background delivery, focus isolation or application success. Keep the native delivery receipt and check the application's result separately. A successful admission consumes the capture even if later native delivery fails.

## Verification

The common capture service owns the atomic endpoint, frame, transform and one-use contract test. Windows tool tests cover its live schema and reject unsupported routes before native work. Exact Windows build and disposable two-monitor application evidence are required before qualifying a runtime. This fork change does not by itself certify the cross-platform desktop matrix.

Decision: [fork RFC #6](https://github.com/alexshpunt/cua/issues/6).
