fn main() {
    println!("cargo:rerun-if-changed=resources/windows/app.rc");
    println!("cargo:rerun-if-changed=resources/windows/CodexImage.ico");

    // `cfg!(target_os)` in a build script describes the host, so the target OS
    // has to come from Cargo's environment for cross builds to embed resources.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("resources/windows/app.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed the Windows icon and application manifest");
    }
}
