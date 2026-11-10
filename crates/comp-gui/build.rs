// Embeds the Windows .exe icon. A no-op on other platforms.
fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=app.rc");
        embed_resource::compile("app.rc", embed_resource::NONE).manifest_optional().unwrap();
    }
}
