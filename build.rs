fn main() {
    // `cfg!(windows)` in a build script describes the machine doing the build, not the target,
    // so ask cargo for the target OS to keep cross-compiles correct.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        println!("cargo:rerun-if-changed=icon.rc");
        embed_resource::compile("icon.rc", embed_resource::NONE).manifest_required().unwrap();
    }
}
