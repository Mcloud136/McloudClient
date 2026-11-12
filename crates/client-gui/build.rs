fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    std::fs::copy(
        "assets/icon.ico",
        std::path::Path::new(&out).join("icon.ico"),
    )
    .expect("copy tray icon");
}
