//! ghostty-sys build.rs：
//! 1. 确保钉版 vendored libghostty 嵌入库已构建（vendor/ghostty/build.sh，
//!    zig 0.15.2 + ghostty a887df42，产物 out/lib/libghostty-internal.a +
//!    out/include/ghostty.h）；
//! 2. bindgen 生成嵌入 API 绑定；
//! 3. 静态链入宿主（框架 + libobjc + libc++）。
//!
//! 环境变量 NINJA_GHOSTTY_EMBED_DIR 可指向现成产物目录（须含
//! lib/libghostty-internal.a 与 include/ghostty.h），跳过 vendored 构建——
//! 仅供本地调试，正式构建走 vendor/。

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/ghostty-sys must live in <workspace>/crates/")
        .to_path_buf();
    let vendor = workspace_root.join("vendor/ghostty");

    // ---- 1. ensure the vendored build exists --------------------------------
    let (lib_dir, include_dir) = match env::var("NINJA_GHOSTTY_EMBED_DIR") {
        Ok(dir) => {
            let dir = PathBuf::from(dir);
            assert_artifacts(&dir);
            (dir.join("lib"), dir.join("include"))
        }
        Err(_) => {
            for f in ["fetch.sh", "build.sh", "xcrun-shim/xcrun"] {
                println!("cargo:rerun-if-changed={}", vendor.join(f).display());
            }
            for p in [
                "patches/0001-darwin-install-static-embed-lib.patch",
                "patches/0002-install-themes-on-embed-route.patch",
            ] {
                println!("cargo:rerun-if-changed={}", vendor.join(p).display());
            }
            let archive = vendor.join("out/lib/libghostty-internal.a");
            // 主题资源（q2：具名 theme= 解析需要）与归档同为构建产物。
            let themes = vendor.join("out/share/ghostty/themes");
            let need_build = !archive.exists()
                || !themes.is_dir()
                || scripts_newer_than(&vendor, &archive);
            if need_build {
                let status = Command::new("bash")
                    .arg(vendor.join("build.sh"))
                    .current_dir(&vendor)
                    .status()
                    .expect("failed to spawn vendor/ghostty/build.sh");
                assert!(status.success(), "vendor/ghostty/build.sh failed");
            }
            assert_artifacts(&vendor.join("out"));
            (vendor.join("out/lib"), vendor.join("out/include"))
        }
    };

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    // 静态链入合并归档（zig bundle_compiler_rt/ubsan_rt + 全部 zig 依赖）。
    println!("cargo:rustc-link-lib=static=ghostty-internal");

    // libghostty darwin 嵌入面直接引用的框架/运行库（由归档未定义符号盘点得来：
    // CT*/CG*/MTL*/CAMetalLayer/IOSurface*/CV*/NSA*/TIS* 等，见审计报告）。
    // rustc 的 -l framework=… 会随 rlib 元数据传到最终二进制链接。
    for fw in [
        "AppKit",
        "Metal",
        "QuartzCore",
        "CoreText",
        "CoreGraphics",
        "CoreVideo",
        "IOSurface",
        "CoreFoundation",
        "Foundation",
        "Carbon",
    ] {
        println!("cargo:rustc-link-lib=framework={fw}");
    }
    println!("cargo:rustc-link-lib=dylib=objc");
    println!("cargo:rustc-link-lib=dylib=c++");

    // ---- 2. bindgen on the installed embed header --------------------------
    let header = include_dir.join("ghostty.h");
    println!("cargo:rerun-if-changed={}", header.display());

    // 本机可能只有 Homebrew 的旧 x86_64 libclang；优先 Xcode/CLT 自带的 arm64 版。
    if env::var_os("LIBCLANG_PATH").is_none() {
        for cand in [
            "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib",
            "/Library/Developer/CommandLineTools/usr/lib",
        ] {
            if Path::new(cand).join("libclang.dylib").is_file() {
                // build.rs 是单线程进程，这里 set_var 安全。
                unsafe { env::set_var("LIBCLANG_PATH", cand) };
                break;
            }
        }
    }

    let bindings = bindgen::Builder::default()
        .header(header.to_str().unwrap())
        .clang_arg("-DGHOSTTY_STATIC")
        // 仅公开嵌入 API：函数/类型/常量都带 ghostty_/GHOSTTY_ 前缀。
        .allowlist_function("^ghostty_.*")
        .allowlist_type("^ghostty_.*")
        .allowlist_var("^GHOSTTY_.*")
        .allowlist_item("^ghostty_.*")
        // 枚举常量不要加 ghostty_clipboard_e_ 前缀（保持头文件原名）。
        .prepend_enum_name(false)
        // bindgen 0.72 默认生成 "raw" layout（repr(C)），不再需要 layout tests。
        .generate_comments(true)
        .generate()
        .expect("bindgen failed on include/ghostty.h (vendored build)");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("failed to write bindings.rs");
}

fn assert_artifacts(dir: &Path) {
    let lib = dir.join("lib/libghostty-internal.a");
    let header = dir.join("include/ghostty.h");
    assert!(
        lib.is_file(),
        "missing {} (run vendor/ghostty/build.sh)",
        lib.display()
    );
    assert!(
        header.is_file(),
        "missing {} (run vendor/ghostty/build.sh)",
        header.display()
    );
}

/// vendor 脚本或补丁比归档新则重建（钉点或脚本变更不会静默沿用旧产物）。
fn scripts_newer_than(vendor: &Path, archive: &Path) -> bool {
    let archive_mtime = archive
        .metadata()
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let mut files = vec![
        vendor.join("fetch.sh"),
        vendor.join("build.sh"),
        vendor.join("xcrun-shim/xcrun"),
        vendor.join("patches/0001-darwin-install-static-embed-lib.patch"),
        vendor.join("patches/0002-install-themes-on-embed-route.patch"),
    ];
    // 钉点源码变了（fetch.sh 校验的 COMMIT 变更）也覆盖 src/build.zig 时间戳。
    files.push(vendor.join("src/build.zig"));
    files.iter().any(|f| {
        f.metadata()
            .and_then(|m| m.modified())
            .map(|t| t > archive_mtime)
            .unwrap_or(false)
    })
}
