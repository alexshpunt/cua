//! Isolated read-only capture experiment. This executable is not a Driver runtime.
#[path = "../src/persistent_wgc/policy.rs"]
mod policy;

#[cfg(target_os = "windows")]
#[path = "persistent_wgc/windows.rs"]
mod native;

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    return native::run();
    #[cfg(not(target_os = "windows"))]
    anyhow::bail!("persistent-wgc-probe requires Windows; no capture was attempted");
}
