fn main() {
    // `cfg(target_os)` in a build script is the host's: check the target, so a cross build embeds it too.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_manifest::embed_manifest_file("cua-driver-uia.manifest")
            .expect("failed to embed cua-driver-uia.manifest");
        println!("cargo:rerun-if-changed=cua-driver-uia.manifest");
    }
}
