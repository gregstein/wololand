// Embeds the app icon as resource #1: Explorer, the taskbar and GPUI's windows all use it,
// and the tray loads it from there too.
fn main() {
    println!("cargo:rerun-if-changed=assets/wololand.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/wololand.manifest");
    embed_resource::compile("assets/wololand.rc", embed_resource::NONE).manifest_optional().unwrap();
}
