# test fixture apps

Each subdirectory is one deterministic host app cua-driver can drive. Apps
share fixtures through `../shared/scenarios.json` and, for webview hosts,
`../shared/web/index.html`.

| Path | Host OS | What it tests |
| --- | --- | --- |
| `cross-platform/electron` | any | Electron/Chromium accessibility and CDP |
| `cross-platform/tauri` | any | Tauri/native-webview accessibility |
| `linux/gtk3` | Linux | GTK3/AT-SPI widgets |
| `macos/appkit` | macOS | AppKit controls and menus |
| `macos/swiftui` | macOS | SwiftUI controls and popovers |
| `macos/wkwebview` | macOS | WKWebView loading the shared DOM |
| `windows/wpf` | Windows | WPF/UIA controls, popups, child HWNDs |
| `windows/winui3` | Windows | WinUI3/XAML controls and popups |
| `windows/webview2` | Windows | WebView2 loading the shared DOM |

Built outputs land in `libs/cua-driver/rust/test-apps/harness-<name>/` via
`../build/<host>`.

On Windows, the Tauri behavioral fixture also writes `fixture-focus.json` beside
each recording. A separate loopback journal receives read-only DOM focus and
keyboard diagnostics every 250 ms. It retains the last 32 focus events and 64
key events, their targets, the active element, document focus and transport
errors. The observer does not focus controls, inject input or gate readiness.
These diagnostics do not change `fixture-state.json` or replace its action and
no-mutation assertions.

Adding a new app:

1. Put native source under `apps/<platform>/<toolkit>/`, or
   `apps/cross-platform/<toolkit>/` when the same source runs on multiple OSes.
2. Reuse `shared/scenarios.json` and `shared/web/index.html` instead of copying
   private ids.
3. Stage into `rust/test-apps/harness-<toolkit>/`.
4. Add or update the matching Rust tests and this table.
