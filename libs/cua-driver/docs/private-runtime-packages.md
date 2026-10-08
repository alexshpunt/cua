# Private MCP runtime packages

The fork's `Native runtime packages` workflow builds one exact source commit for
Windows x64, Linux x64, macOS Intel and macOS Apple Silicon. It creates npm
archives for a downstream private extension installation. It does not publish,
install a service, change PATH or use official signing credentials.

Dispatch `.github/workflows/native-runtime-build.yml` on the work branch with a
full `source_ref` commit SHA. All four jobs must pass before the complete-set job
produces its manifest and four npm archives. Do not distribute a partial set.
Each package carries its source, version, binary and notice hashes, required
companions, and the read-only startup/schema probe. Version/help, initialize and
tools/list checks do not prove that capture or input works on a real desktop.
Each native job also installs its npm archive into an isolated directory, checks
installed metadata and every payload hash against the original package, then
repeats the read-only startup probe from that installed path. Missing installed
package/startup evidence blocks the complete-set gate.

The Windows package deliberately permits installation on Linux x64 too. npm sees
WSL as Linux; this lets the extension select Windows-host control without a
second installation. Ordinary Linux uses the Linux package instead. The resolver
must verify the chosen execution target and hashes before starting the runtime.

## Build boundaries

The package gate runs the common core/contract unit suites on every target and
Windows native owner tests on Windows. Linux/macOS native desktop suites are not
certification gates here. An initial macOS platform suite passed on one run but
aborted in an AppKit weak-reference path on the next; do not turn that into a
platform test-pass claim. Real desktop certification remains separate.

- Windows packages include the driver, cursor-theme CLI and UIAccess helper.
  These are unsigned development artifacts; including the helper does not grant
  UIAccess permission.
- Linux builds in Debian 11, with a glibc 2.31 floor and the same X11/Wayland
  dependencies as the upstream portable release. Portal input is enabled.
  PipeWire ScreenCast is not enabled in this portable build; do not claim every
  Wayland compositor supports window capture or input.
- Each macOS architecture builds and starts on its own macOS 26 runner, with a
  deployment target of macOS 13. These packages are not notarized or signed with
  a developer identity. Accessibility and Screen Recording approval remain OS
  requirements. CI cannot establish live permission behavior or app parity.
- Root MIT licensing and Rust third-party notices travel with every runtime.
  The separate overlay and model environments are not included.

The upstream public release pipeline and installers are unchanged. Any future
native lifetime or permission contract change needs its own recorded decision;
this workflow does not establish one.
