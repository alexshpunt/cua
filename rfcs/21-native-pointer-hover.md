---
title: Exact-window native pointer move and bounded hover
authors:
  - alexshpunt
  - alexshpunt-clanker[bot]
created: 2026-10-10
last_updated: 2026-10-10
status: accepted
discussion: https://github.com/alexshpunt/cua/issues/21
rfc_pr:
implementation: []
supersedes:
superseded_by:
---

# RFC: Exact-window native pointer move and bounded hover

## Summary

Add experimental Windows `move_pointer` to this fork. Move the real pointer
without button or key events, with optional bounded hover dwell. Keep existing
`move_cursor` unchanged. The [discussion issue](https://github.com/alexshpunt/cua/issues/21)
records the full contract and owner decision.

## Motivation and current state

Windows `move_cursor` moves an overlay by default. Its desktop branch uses
global `SetCursorPos`; it is not a capture-bound native window action. Agents
need native motion to open hover menus and tooltips without clicking.

## Goals and non-goals

Support one exact window and either capture-bound local pixels or a current
element token. Bound dwell and cancellation, retain input admission until the
worker stops, and report delivery separately from app effects.

Do not add scroll, window management, CDP, mixed batches, semantic waits,
non-Windows native implementations, production installation or publication.

## Proposal

`move_pointer` requires `pid` and `window_id`. Accept either `capture_id`, `x`
and `y`, or `element_token`, never both. `dwell_ms` is an integer from 0 to
10000; zero moves only. The integrating agent exposes separate move/hover
intents. Common Rust code owns the request, timing and receipt contract.

Background is the default and refuses before activation or pointer movement.
Explicit foreground may activate the exact window and insert one absolute
mouse move. Validate identity, visibility, geometry, held buttons, integrity
and the actual hit window before insertion. Never fall back to desktop input,
posted events, another window, buttons or keys.

Leave foreground and pointer at the target after delivery. During dwell, stop
on cancellation, target/focus/geometry loss or user pointer movement. Never
repeat movement to fight user input. Cancellation cannot retract inserted OS
input. A receipt records the attempted/delivered prefix, elapsed dwell and
focus/pointer effects; it does not claim that a tooltip appeared. Observe again.

Only Windows advertises this experimental tool. Other platforms' existing
`move_cursor` is not evidence of this capability.

## Alternatives considered

Overlay movement has no native effect. Global cursor calls lack exact-window
admission. Posted `WM_MOUSEMOVE` does not move the real pointer and misses many
tooltip stacks. Restoring focus or pointer can dismiss hover before readback.

## Compatibility and migration

This additive experiment starts from `windows-semantic-actions` at
`b28749f47494d0fb5150e9f377a46605928cc8b6`, whose native code matches the
selected WGC runtime source. No existing tool changes meaning. Build and
qualify an isolated exact-SHA candidate; adoption is a separate owner decision.

## Security, privacy, and telemetry

Reuse capture/token admission, exact ownership and retained session input
admission. Operate only disposable test windows. Use synthetic evidence, not
private desktop content or raw transcripts. Add no telemetry.

## Implementation and acceptance plan

1. Add hermetic request, bounded timing and interrupted-receipt contract tests.
2. Add the thin Windows native adapter and exact refusal/admission coverage.
3. Build an isolated Windows candidate. Verify two-window hover, no button/key
   events, background refusal, foreground effects, moved-window mapping,
   changed-size rejection, occlusion, cancellation and target loss.
4. Adapt the Pi extension with real-loader regressions and live Codemode/TUI
   evidence. Do not change production runtime selection without approval.

Follow the repository's exact-SHA desktop certification policy before readiness
or merge. Diagnostic smokes do not replace the canonical matrix.

## Unresolved questions

Native tooltip behavior and platform limitations still need qualification.
No unresolved product choice remains for the first candidate.

## Decision record

Accepted for a focused personal-fork candidate. The owner approved native fork
work, leaving the target active with the pointer at the target, and background
refusal instead of posted-event substitution. This selects implementation,
not merge, adoption or publication. The short review window applies only to
this additive personal-fork experiment; upstream review remains separate.
