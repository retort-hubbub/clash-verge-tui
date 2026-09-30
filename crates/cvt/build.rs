//! Record the full compilation target so updater assets preserve architecture and ABI.
fn main() {
    let target = std::env::var("TARGET").expect("Cargo provides TARGET");
    println!("cargo:rustc-env=CVT_BUILD_TARGET={target}");
}
