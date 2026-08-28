use std::env;
use std::path::PathBuf;

fn main() {
    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    // 资源（应用图标 + SxS 清单）已预编译为 resources/deskfence.res 提交在
    // 仓库里：源码构建零外部工具依赖（不再需要 MinGW windres），GitHub
    // Actions 的 windows runner 也能直接 cargo build。
    // 修改图标/清单后本地重新生成一次并提交：
    //   windres --input DeskFence.rc --output-format=res --output resources/deskfence.res
    println!("cargo:rerun-if-changed=resources/deskfence.res");
    println!("cargo:rerun-if-changed=DeskFence.rc");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/deskfence.ico");

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set"));
    let res = manifest_dir.join("resources").join("deskfence.res");
    assert!(
        res.exists(),
        "resources/deskfence.res is missing (see comment in build.rs to regenerate it)"
    );
    println!("cargo:rustc-link-arg={}", res.display());
}
