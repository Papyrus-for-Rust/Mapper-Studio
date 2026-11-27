fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // Windows' 1 MiB executable default is smaller than Rust's 2 MiB
        // worker stacks. Bounded debug composition exceeds the former.
        println!("cargo:rustc-link-arg=/STACK:2097152");
        let icon = "assets/branding/papyrus.res";
        println!("cargo:rerun-if-changed={icon}");
        let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
        let resource = std::path::PathBuf::from(manifest_dir).join(icon);
        println!("cargo:rustc-link-arg-bin=mapper-studio={}", resource.display());
    }
}
