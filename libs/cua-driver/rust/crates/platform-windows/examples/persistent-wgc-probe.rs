// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Aleksandr Shpuntenko

//! Isolated read-only capture experiment. This executable is not a Driver runtime.
// The probe uses only the frame policy; the shared module also defines Driver-only crop policy.
#[allow(dead_code)]
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
