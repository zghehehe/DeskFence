use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    println!("cargo:rerun-if-changed=DeskFence.rc");
    println!("cargo:rerun-if-changed=assets/deskfence.ico");
    println!("cargo:rerun-if-changed=app.manifest");

    let output =
        PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set")).join("deskfence.res");
    let status = Command::new("windres")
        .args(["--input", "DeskFence.rc", "--output-format=res", "--output"])
        .arg(&output)
        .status()
        .expect("windres must be available to compile the DeskFence icon resource");

    assert!(status.success(), "windres failed to compile DeskFence.rc");
    println!("cargo:rustc-link-arg={}", output.display());
}
