---
title: Strict Windows semantic element actions
authors:
  - alexshpunt
created: 2026-10-08
last_updated: 2026-10-08
status: accepted
discussion: https://github.com/alexshpunt/cua/issues/10
rfc_pr: https://github.com/alexshpunt/cua/pull/11
implementation:
  - https://github.com/alexshpunt/cua/pull/11
supersedes:
superseded_by:
---

# Strict Windows semantic element actions

## Decision

The fork owner approved this bounded Windows implementation in
[the decision record](https://github.com/alexshpunt/cua/issues/10#issuecomment-6055883141).
This is not upstream acceptance or permission to merge without desktop evidence.
The issue contains the proposal, alternatives, migration and acceptance plan.

## Contract

Add `semantic_action` with exact pid, window_id, current element_token, operation
(invoke, select or set_value), optional session and a value for set_value only.
No coordinates, activation or foreground option are accepted.

The Windows adapter uses existing admitted UIA targets and the provider
EnableWindow focus shield. It does not add WS_EX_NOACTIVATE: live testing found
that restoring that style can move the target to the current shell desktop.
Qualification checks desktop membership before and after every action.
Invoke uses InvokePattern; select uses SelectionItemPattern.Select; set_value
uses ValuePattern or RangeValuePattern. Failed dispatch never retries through
mouse messages, keys, SendInput or a different input route. A missing pattern,
disabled target, invalid value or stale token has a concrete refusal. An attempted
provider failure stays unknown, not a claim that nothing happened.

A receipt states the operation and pattern. Provider completion does not prove
an application effect or unchanged focus. Independent synthetic app readback and
a foreground sentinel qualify each supported app/control/action combination on
the current and another existing shell desktop.

Only Windows advertises this operation initially. Other platforms do not
substitute a weaker implementation. Ordinary click and set_value stay unchanged.
The agent adapter adds explicit verbs inside computer_act, not a new tool family.

## Delivery gates

Build from the qualified single-PNG source c2d84f09, preserving its capture work.
Use hermetic contract/admission tests and exact Windows build/schema/hash checks.
Candidate use and permanent selection need separate owner decisions. Production
Pi reload, real app effects, focus checks, TUI inspection and owned fixture cleanup
are required. Physical focus policy, desktop lifecycle, multi-agent arbitration,
browser backends and capture changes are outside this proposal.

This scoped fork qualification is not the complete cross-platform E2E matrix
required for an upstream-ready or merge claim.
