fn main() {
    // Release builds link the C runtime statically, so the app runs without
    // the VC++ redistributable. Debug builds don't: tauri-build does it by
    // putting a stub msvcrt.lib on the link path, and cargo also passes that
    // path to the other workspace crates' doctests, which then fail to link.
    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let windows = tauri_build::WindowsAttributes::new().static_vc_runtime(release);
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("tauri-build failed");
}
