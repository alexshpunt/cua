# What this fork changes

This is a fork of [trycua/cua](https://github.com/trycua/cua). It adds Windows features to Cua Driver that [Pi Agent Computer Use](https://github.com/alexshpunt/pi-agent-computer-use) needs and upstream doesn't have yet. Everything else is upstream's code, under upstream's license.

The `pi-computer-use` branch is the build Pi Agent Computer Use ships: cua-driver 0.37.0 from commit `2437a85e`. Its binaries are attached to the [`pi-runtime-0.37.0-2437a85e` release](https://github.com/alexshpunt/cua/releases/tag/pi-runtime-0.37.0-2437a85e) and come from the `windows-fork-build.yml` workflow. They are unsigned. Pi Agent Computer Use pins the SHA-256 of every file and checks it before it starts the driver.

All changes are in `libs/cua-driver` and only touch Windows. macOS and Linux behave like upstream.

## Input

- Held drag paths: a left-button stroke through up to 254 points, released at the end of the call. Interrupted paths report which points were delivered. This is what makes continuous brush strokes in Paint possible.
- Key holds: hold up to 8 keys while a bounded set of clicks and drags runs, then release them in the same call.
- `input_timeline`: timed key, button, move, raw relative move, wheel and pen events in one call, with pen pressure and tilt. Everything still held is released when the call ends or fails.
- Pen events of every `input_timeline` in a driver process come from one synthetic pen, so apps that keep settings per device see one pen. Pen frames out of contact (in range before contact, lift-off and after) carry no pressure.
- Native pointer move and hover with a dwell time on an exact window.
- Coordinate wheel scroll at a point of the last capture, in wheel detents.
- Straight drags are bound to the capture they were planned on.
- Foreground clicks and drags go through `SendInput` and keep the target window active. Clicks on a target's owned popups work.
- A foreground press checks its point first: a click, the start of a drag or held path, and each button or pen press of an `input_timeline`. When after activation another top-level window still covers the point, the call refuses with `pointer_target_occluded` before pressing, and names that window's id, pid, title and relation (`owned_window`, `same_process` or `other_process`). The target's own captionless popups, such as menus and drop-downs, take the press.
- A foreground click that closes its target, such as OK in a dialog, succeeds when a visible window of the same process takes the foreground; the result names it as `foreground_after`. The target counts as closed once it is destroyed or hidden. The check after the click waits up to 300 ms for Windows to hand the foreground on. A foreground of another process still fails.
- Foreground input pauses while the person at the machine uses the keyboard or mouse, and resumes 10 seconds after they stop.
- Text is typed as keystrokes when the target's keyboard layout has the character.
- Strict UI Automation actions: `invoke`, `select` and `set_value` on an element, without falling back to mouse or keyboard, and without moving the human's focus or switching virtual desktops.
- `click` and `scroll` take `uia_only`: an element is pressed or scrolled only through its UI Automation patterns, and when none carries the action the call refuses with the cause before sending anything. Upstream falls back to posted window messages, which some apps (Windows Terminal, XAML islands) drop while the call still reports success.

## Windows and displays

- Restore a minimized window, and move, resize, minimize, maximize or focus an exact window, with a receipt of what actually changed.
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

## Protocol

- An EOF-aware MCP receiver, so a host that closes stdin still gets the receipt of an interrupted held path.
- Input schemas stay inside the JSON Schema subset that Vertex AI accepts.
- `launch_app` can start a browser with remote debugging when the host sets `CUA_DRIVER_ALLOW_DEBUGGING_LAUNCH=1`. Pi Agent Computer Use sets it only in its YOLO mode and then attaches Playwright to that browser.

## Upstream

These changes are made for one consumer and tested on Windows 11 from WSL. Some of them could go upstream; none has been offered as a pull request yet.
