fn main() {
    println!("cargo:rerun-if-changed=../../packaging/openrazer.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("../../packaging/openrazer.ico")
            .set("FileDescription", "openrazer-win command line")
            .set("ProductName", "openrazer-win")
            .compile()
            .expect("embedding Windows resources");
    }
}
