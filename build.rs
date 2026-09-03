fn main() {
    println!("cargo:rerun-if-changed=resources/windows/app.rc");
    println!("cargo:rerun-if-changed=resources/windows/CodexImage.ico");

    #[cfg(target_os = "windows")]
    embed_resource::compile("resources/windows/app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed the Windows icon and application manifest");
}
