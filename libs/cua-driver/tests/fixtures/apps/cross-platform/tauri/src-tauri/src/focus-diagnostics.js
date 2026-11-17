// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

// Passive diagnostics use a separate receiver from the fixture state oracle.
(() => {
  const endpoint = window.__CUA_E2E_FIXTURE_FOCUS_JOURNAL_URL;
  if (!endpoint) return;

  const focusEvents = [];
  const keyEvents = [];
  let sequence = 0;
  let transportErrors = 0;
  let lastTransportError = null;
  let publishing = false;
  const identity = (element) => ({
    id: element?.id || null,
    tag: element?.tagName || null,
  });
  const remember = (events, limit, event) => {
    events.push({
      sequence: ++sequence,
      timestamp: Date.now(),
      type: event.type,
      trusted: event.isTrusted,
      target: identity(event.target),
      active: identity(document.activeElement),
      hasFocus: document.hasFocus(),
      ...(event instanceof KeyboardEvent
        ? {
            key: event.key,
            code: event.code,
            repeat: event.repeat,
            shift: event.shiftKey,
            ctrl: event.ctrlKey,
            alt: event.altKey,
            meta: event.metaKey,
          }
        : {}),
    });
    if (events.length > limit) events.splice(0, events.length - limit);
  };
  for (const type of ["focusin", "focusout"]) {
    document.addEventListener(type, (event) => remember(focusEvents, 32, event), true);
  }
  for (const type of ["focus", "blur", "load"]) {
    window.addEventListener(type, (event) => remember(focusEvents, 32, event));
  }
  for (const type of ["keydown", "keyup"]) {
    document.addEventListener(type, (event) => remember(keyEvents, 64, event), true);
  }

  const publish = async () => {
    if (publishing) return;
    publishing = true;
    try {
      const response = await fetch(endpoint, {
        method: "POST",
        headers: { "Content-Type": "text/plain" },
        body: JSON.stringify({
          timestamp: Date.now(),
          sequence,
          readyState: document.readyState,
          hasFocus: document.hasFocus(),
          active: identity(document.activeElement),
          focusEvents,
          keyEvents,
          transportErrors,
          lastTransportError,
        }),
      });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
    } catch (error) {
      transportErrors++;
      lastTransportError = String(error);
    } finally {
      publishing = false;
    }
  };
  // No focus calls, input injection, per-key requests or readiness gates.
  window.addEventListener("load", () => {
    void publish();
    window.setInterval(publish, 250);
  }, { once: true });
})();
