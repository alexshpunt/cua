# What this fork changes

This fork integrates the full upstream [trycua/cua](https://github.com/trycua/cua) history through Cua Driver 0.52.0 (upstream `main` at `ca6a6434`) while retaining the Windows Cua Driver extensions used by [Pi Agent Computer Use](https://github.com/alexshpunt/pi-agent-computer-use). Upstream code retains its original authorship and license.

The current source of Pi Agent Computer Use selects Windows Cua Driver 0.52.0 built from commit `3cf8455bfbd5b5b20231b4f24de7b6b6ad2c0322`, attached to the [`pi-runtime-0.52.0-3cf8455b` release](https://github.com/alexshpunt/cua/releases/tag/pi-runtime-0.52.0-3cf8455b). These unsigned AMD64 binaries were built on GitHub's Windows runner with a static CRT. The package pins and checks the SHA-256 of every file before starting the driver. This does not establish UIAccess deployment, elevated/UWP support or physical display coverage.

Where upstream and the fork change the same behavior, this build keeps both:

- The desktop lane is upstream's `DesktopLease`: a call waits behind a worker that outlived its call for at most 12 s, then refuses with `input_busy`. The fork's native workers keep the lease until they stop.
- Typed input is batched as upstream does, with the fork's layout keystrokes and stop checks. When Windows has not confirmed that the target read inserted input, the target stays in front (upstream #6779), and focus return leaves it there too.
- `move_pointer` and `scroll_at` keep their own closed receipts instead of upstream's `ActionResult`; each reports an explicit `effect`.
- The HTTP transport requires the `Mcp-Session-Id` that `initialize` returns.
- The Windows tools follow upstream's one-module-per-tool layout; `hold_keys` and `input_timeline` have their own modules.

The native macOS desktop matrix was not rerun for this build.

## Input

- Held drag paths: a left-button stroke through up to 254 points, released at the end of the call. Interrupted paths report which points were delivered. This is what makes continuous brush strokes in Paint possible.
- Key holds: hold up to 8 keys while a bounded set of clicks and drags runs, then release them in the same call.
- `input_timeline`: timed key, button, move, raw relative move, wheel and pen events in one call, with pen pressure and tilt. Everything still held is released when the call ends or fails.
- Pen events of every `input_timeline` in a driver process come from one synthetic pen, so apps that keep settings per device see one pen. Pen frames out of contact (in range before contact, lift-off and after) carry no pressure.
- Native pointer move and hover with a dwell time on an exact window.
- Coordinate wheel scroll at a point of the last capture, in wheel detents.
- Straight drags are bound to the capture they were planned on.
- Foreground clicks and drags go through `SendInput` and keep the target window active, unless focus return (below) is on. Clicks on a target's owned popups work.
- A window of another process embedded in the target, one whose parent chain reaches it (such as a Unity Editor that Herdr shows in a tab), counts as the target. A foreground click that gives it the foreground succeeds, and while it has the foreground, keys and text for the target go to it instead of activating the target and taking the keyboard from it.
- A foreground press checks its point first: a click, the start of a drag or held path, and each button or pen press of an `input_timeline`. When after activation another top-level window still covers the point, the call refuses with `pointer_target_occluded` before pressing, and names that window's id, pid, title and relation (`owned_window`, `same_process` or `other_process`). The target's own captionless popups, such as menus and drop-downs, take the press.
- A foreground click that closes its target, such as OK in a dialog, succeeds when a visible window of the same process takes the foreground; the result names it as `foreground_after`. The target counts as closed once it is destroyed or hidden. The check after the click waits up to 300 ms for Windows to hand the foreground on. A foreground of another process still fails.
- Foreground input never waits for the person at the machine: it takes the foreground and runs.
- With `CUA_FOCUS_RETURN=1`, every foreground input call on a window, named by `window_id` or `target` or resolved from a pid alone (click, double and right click, drag, held keys, `input_timeline`, typing, keys, hotkeys, scroll and pointer moves) gives the foreground back when it is over. It goes to the human's window: the window in front when the person last pressed a physical key or mouse button, which low-level hooks count; injected input, the agent's own included, does not count. The foreground stays where it is when the human's window already has it, when a window that is not the target's took it during the call, when a captionless popup of the target's process that the call opened (or an earlier call opened and kept) is still open, such as a menu or a drop-down, and when the human's window is closed, hidden, minimized or on another virtual desktop. A popup gets up to 150 ms to appear. The call's result then gets a text line `focus_return: <outcome> window_id=…`, with outcome `restored`, `refused`, `kept_for_popup`, `human_window_gone`, `human_moved` or `other_window`. Key and text delivery in those calls no longer hand the foreground back to the window that was in front before them; keys addressed only by an element token still do.
- With `CUA_ESCAPE_STOP=1`, three physical Escape presses within a second stop all input of the driver process. Injected Escape, including the agent's own, does not count, and auto-repeat counts once. Every tool that is not read-only then refuses with `human_stop`, running typing, held paths, timelines and pointer moves stop at their next step, and the envelope transport sends `notifications/cua/human_stop` at once. The stop lasts until the process exits.
- With `CUA_SHARED_INPUT_LOCK=1`, each desktop action holds an exclusive lock on `cua-driver-shared-input.lock` in the user's temporary directory, so agents in separate driver processes on one desktop never interleave inside an action. A call waits up to 60 s for another process's action, then refuses with `input_busy`. Windows releases the lock when a process exits.
- Background input (`delivery_mode:"background"`, the default) on Windows follows a route matrix measured on real apps with another window in front and the cursor away (LPT-1846):
  - Modifiers (and, for clicks and drags, their `MK_*` flags) are held in the target UI thread's key state through `AttachThreadInput` + `SetKeyboardState` while the posted messages are processed, then restored; no modifier key messages are posted, and the global key state does not change. Click, right click, double click and drag accept `modifier` in the background.
  - Keys and text go to the target thread's focused child, else to a XAML island's input site or a UWP CoreWindow; Chromium and Electron frames take them on their top-level window, after a posted activation that wakes their keyboard once they have been in the foreground.
  - A key that leaves the target in a Win32 menu loop, which would stop a background app, is cancelled with `WM_CANCELMODE` and refused as `background_unavailable`.
  - `scroll_at` posts wheel messages to the window at the point; the pointer does not move.
  - Classes that take posted input: classic Win32, Unity, Chromium and Electron (clicks of any shape, drags, keys, chords, text), GTK, Godot. Refused before input, so the caller can take the foreground: pointer input on XAML islands and UWP surfaces, pointer input on WPF (it needs the real cursor over the window), everything but the wheel on Blender, keys on Tk and VCL.
  - A background call on an exact window does not take the shared input lock: it waits for no other agent's foreground action.
- `claim_window` holds an exact window for the calling driver process, and `release_window` gives it back. While a process holds a window, every other driver process on the desktop refuses tools that change it (input, `set_value`, semantic actions, `bring_to_front`, restore and management, `set_window_frame`, `invoke_menu`) with `window_claimed`, the holder name the claim gave and nothing sent; reads still work. A claim is an exclusive lock on `<window_id>-<pid>.lock` in `cua-driver-window-claims` under the user's temporary directory, so Windows frees it when the process exits, and a reused window handle of another process is a different claim. A call that names its window only by an element token is not checked.
- Text is typed as keystrokes when the target's keyboard layout has the character.
- Strict UI Automation actions: `invoke`, `select` and `set_value` on an element, without falling back to mouse or keyboard or intentionally activating the target. Provider focus behavior still needs independent verification.
- Strict UIA actions admit a cross-process element only when its provider PID still matches the observed snapshot and its current UIA ancestry reaches the exact target window. This permits embedded WebView2 controls without retargeting to another window.
- `click` and `scroll` take `uia_only`: an element is pressed or scrolled only through its UI Automation patterns, and when none carries the action the call refuses with the cause before sending anything. Upstream falls back to posted window messages, which some apps (Windows Terminal, XAML islands) drop while the call still reports success.

## Windows and displays

- Restore a minimized window, and move, resize, minimize, maximize or focus an exact window, with a receipt of what actually changed. A resize may land up to 2 px away in width or height and still counts as done: Windows keeps a window that is not DPI aware in whole logical pixels on a scaled display, so some sizes are unreachable. Position and every other operation stay exact.
- Focus activates a window while another app holds the foreground the same way input does, with a no-op key event that gives the driver foreground rights.
- Read a selected display with its native topology, not only the primary one.
- Report which virtual desktop a window belongs to.
- Confirm that a window hasn't changed from its frame size instead of capturing it again before each capture-bound action.

## Structural tree

- `get_window_state` lists every row the UIA walk emits, not only actionable ones. Static text, disabled controls and named containers get an `element_index`, an `element_token`, a `frame` and become the `parent_index` of their children. `actionable` says which rows have action patterns; read-only rows keep an empty `actions`.
- Each element carries the raw UIA `name`, `automation_id`, `class_name` and `help_text`, and, when UIA reports them, `checked` (`on`/`off`/`mixed`, separate from `selected`), `expanded`, `offscreen`, `focused`, `read_only`, `password`, `required` and `range` (`value`/`min`/`max`). They come from the same single cached walk.
- `elements_complete` is `true` when the walk visited the whole control view: no node or time budget stop, no depth cut and a successful bulk fetch. Then an element missing from the list is absent. MSAA and error paths stay `false`.
- This is the tree a Playwright-style locator engine resolves against.

## Capture

- Window overviews are resized before PNG encoding, with fast Bilinear resizing.
- Process metadata is read once per process ID.
- Capture returns opaque pixels when no desktop compositor runs, as in isolated agent sessions.
- A DPI-virtualized window (DPI-unaware, or system-aware on a monitor with another DPI) is captured the way the desktop compositor shows it: stretched to its physical frame. Upstream's PrintWindow capture leaves a DPI-unaware window at 2/3 size in the image's top-left corner on a 150% display, while UI bounds and input use the physical frame.
- `get_window_state` takes a `crop` rectangle for display-only reads: only that part of the window, in its physical pixels, is copied, resized and PNG-encoded, and the result names it as `screenshot_crop`. Pi Agent Computer Use watches a viewport this way. A crop is never an input source and is refused with the retained WGC route.
- An opt-in retained Windows Graphics Capture route. Pi Agent Computer Use tried it and dropped it, because on the machines it was measured on it was slower than the default route.

## cua-spacesd window streams

- A cua-spacesd window stream whose window Windows Graphics Capture cannot open falls back to `PrintWindow` polling. Headless sessions, such as MXC isolated agent sessions, start no per-user capture service, so WGC fails there with `ERROR_SERVICE_DOES_NOT_EXIST`. The fallback paints the window at most `max_fps` times a second, crops it to the extended frame bounds and publishes a frame only when its pixels change. `GetCapabilities` names the limitation on `window_stream` when the session has no capture service.
- A display target needs an attached display device. A headless session reports a monitor whose screen is always black; it now offers no display target, and `desktop_stream` says that no display is visible.
- Windows display and window streams honor `max_dimension` for H.264 and BGRA without upscaling or changing native input coordinates. H.264 also fits within OpenH264's 3840-pixel long edge and 2160-pixel short edge, even when no cap is requested.
- Measured on ZinPC (Windows 11 26200, NASM-built OpenH264) in an MXC isolated session, through a guest-to-host loopback relay: a 60 Hz window streams at 30 fps with `max_fps` 30 (0.33 Mbit/s, 7 % of one core) or at 58 fps with `max_fps` 60 (0.58 Mbit/s, 15 %); a frame reaches the host 8–10 ms after the window painted it; an unchanged window sends a frame every 3 s; three windows at once cost 17 % of one core.
- The Windows build is `.github/workflows/windows-fork-spacesd-build.yml`. It installs NASM, which GitHub's Windows image lacks; without it the OpenH264 build script silently builds a C-only encoder, which took 25 % of one core instead of 7 % for the same 30 fps window stream.

## Protocol

- An EOF-aware MCP receiver, so a host that closes stdin still gets the receipt of an interrupted held path.
- Input schemas stay inside the JSON Schema subset that Vertex AI accepts.
- `launch_app` can start a browser with remote debugging when the host sets `CUA_DRIVER_ALLOW_DEBUGGING_LAUNCH=1`. Pi Agent Computer Use sets it only in its YOLO mode and then attaches Playwright to that browser.

## Native activity presentation

The published `3e69266e` Windows runtime includes session-owned native activity presentation through `set_agent_visuals` and `present_agent_activity`, with their canonical typed Rust SDK and UniFFI methods in the matching source. The default embedded SDK constructor does not start a native overlay: settings can be configured while presentation returns `visual_session_unavailable`. Admission requires a renderer-owning host, such as the native daemon; configuration alone does not prove that anything was shown.

`CUA_SESSION_LABEL` names the agent cursor of every session in the process that has no public label of its own. `present_agent_activity` takes `typed_text`, at most 200 characters without control characters, and only on an action: the card shows its end. A card stays until the next one replaces it, so consecutive actions do not blink; a card nothing replaces stays about two seconds and fades out over 500 ms.

Input does not wait for cursor animation. Each session has bounded visual playback, with at most 32 pending commands and a 500 ms stale-event budget. Consecutive navigation coalesces and dense activity plays in compressed form. These presentation timings never change actual held input or timeline timing.

Observation frames and operation-output thumbnails use the existing tiny-skia theme palette and typography. They live in separate input-transparent, nonactivating Win32 surfaces. Capture exclusion is permanent and must succeed before showing a surface; the cursor keeps its existing recorder-visibility policy on its own window. Window previews require exact HWND, PID, stamped lifetime property and DWM bounds, rechecked while visible. Display previews require exact current display identity and bounds. Unsupported protection or stale geometry suppresses the visual without changing capture or input.

The default `full` preset enables a short-lived preview; `subtle` omits image previews and `off` clears presentation. Observation, preview, pointer, scroll/drag and keyboard categories are independent. Settings also include HUD opacity, performance/balanced/high cadence and reduced motion. Literal typed text is never part of the activity protocol. Images stay in memory, decode under a 64 MiB allocation limit, shrink to at most 240 by 160 pixels and expire after 1.2 seconds. At most four source surfaces are active, each bounded to 8 Mi physical pixels. No HTTP viewer or image archive is started.

Activity generations reject stale events and invalidate older previews on clear. A queued receipt means admission only; it does not confirm visibility, input delivery or a verified effect. Source validation, image decoding and native rendering run on the presentation worker, outside the input/capture path.

## Upstream

These changes are made for one consumer and tested on Windows 11 from WSL. Some of them could go upstream; none has been offered as a pull request yet.
