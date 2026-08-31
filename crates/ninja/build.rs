//! ninja build.rs：把 vendored ghostty 资源目录（含 themes/，
//! vendor/ghostty/patches/0002 装出）烘进二进制——运行时在 ghostty_init
//! 前设 GHOSTTY_RESOURCES_DIR，具名 `theme =` 才能解析（见 src/config.rs
//! ensure_resources_dir）。目录不存在（如 NINJA_GHOSTTY_EMBED_DIR 指向
//! 外部产物）烘空串，运行时不设环境变量，主题解析失败会进诊断。

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = manifest
        .join("../../vendor/ghostty/out/share/ghostty")
        .canonicalize()
        .unwrap_or_default();
    let baked = if dir.join("themes").is_dir() {
        dir.to_string_lossy().into_owned()
    } else {
        String::new()
    };
    println!("cargo:rustc-env=NINJA_GHOSTTY_RESOURCES_DIR={baked}");
    // themes 出现/变化时重烘（首建时 ghostty-sys 先行，out/ 已就位）。
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("../../vendor/ghostty/out").display()
    );
}
