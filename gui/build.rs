// Embeds the app icon into the Windows EXE (Explorer, taskbar, Alt+Tab).
fn main() {
    println!("cargo:rerun-if-changed=../assets/icon/tast2controller.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../assets/icon/tast2controller.ico");
        if let Err(e) = res.compile() {
            panic!("failed to embed the app icon: {e}");
        }
    }
}
