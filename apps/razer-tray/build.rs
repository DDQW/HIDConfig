fn main() {
    slint_build::compile("ui/settings.slint").expect("compiling settings.slint");

    // Icon + version info in the .exe (Explorer, Task Manager, Apps & features).
    println!("cargo:rerun-if-changed=../../packaging/openrazer.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("../../packaging/openrazer.ico")
            .set("FileDescription", "openrazer-win tray")
            .set("ProductName", "openrazer-win")
            .compile()
            .expect("embedding Windows resources");
    }
}
