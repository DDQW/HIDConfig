fn main() {
    println!("cargo:rerun-if-changed=../../packaging/hidconfig.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("../../packaging/hidconfig.ico")
            .set("FileDescription", "HIDConfig command line")
            .set("ProductName", "HIDConfig")
            .compile()
            .expect("embedding Windows resources");
    }
}
