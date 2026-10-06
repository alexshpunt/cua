# Bounded Windows key holds

`hold_keys` uses one foreground native worker, not a series of complete key calls.
It requires exact `pid` and `window_id`, an earlier window screenshot, 1..8 unique
keys and `duration_ms` between 1 and 10000. Ordinary letters/digits, navigation,
F1..F12 and ctrl/shift/alt/win are accepted. Modifiers go down before ordinary keys.

Optional `actions` contains 1..32 complete pixel left clicks or drags. Coordinates
are local to that earlier screenshot and scale once. A click uses a 50ms press;
a drag supports straight `steps` or intermediate `via` points. Requested pointer
time must fit the hold interval. Each pointer gesture releases the mouse button;
keys stay down for the enclosing command. No nested key actions or cross-call holds.

The native worker keeps the desktop input lease until cleanup finishes. It refuses
already-down requested keys/modifiers/buttons before activation, checks exact
owner, foreground, visibility, cancellation and current point bounds, and releases
only keys inserted by this worker in reverse order. Windows cannot distinguish a
human pressing the same key after the agent injected it; this is not input isolation.

Timing uses a monotonic clock with target/cancellation checks at most 10ms apart
while waiting. There is a small scheduling allowance before stopping pointer work;
requested timing is not a real-time guarantee. Public summaries distinguish requested
hold time, measured held time and total call time, plus completed pointer prefixes.
Input delivery and accepted key-up insertion are not proof of the app's result.

Use the existing acknowledged envelope receiver (`CUA_DRIVER_MCP_ENVELOPES=1`)
for in-flight EOF cancellation. Default serial MCP and cancellation notifications
are not a safe release boundary. Forced process termination cannot run cleanup.

This feature needs a Windows build and real native lifecycle checks before adoption.
The existing qualified mouse-path build stays installed until those checks pass.
