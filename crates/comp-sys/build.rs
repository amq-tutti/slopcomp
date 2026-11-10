//! Links the static native deps produced by `deps/build.sh` and generates bindings.
//!
//! The deps prefix is taken from `ANIME_COMP_DEPS`, defaulting to `<workspace>/deps/prefix`.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let prefix = env::var_os("ANIME_COMP_DEPS")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../deps/prefix"));
    let prefix = prefix
        .canonicalize()
        .unwrap_or_else(|_| panic!("deps prefix {} not found; run deps/build.sh first", prefix.display()));
    // canonicalize() yields \\?\C:\... on Windows, which clang and the linker don't handle well.
    let prefix = PathBuf::from(prefix.to_string_lossy().trim_start_matches(r"\\?\").to_string());
    println!("cargo:rerun-if-env-changed=ANIME_COMP_DEPS");
    println!("cargo:rerun-if-changed={}", prefix.join("lib").display());
    println!("cargo:rerun-if-changed=wrapper.h");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    link(&prefix, &target_os);
    bindings(&prefix);
}

fn link(prefix: &PathBuf, target_os: &str) {
    let pc_dir = prefix.join("lib/pkgconfig");
    let out = Command::new(env::var("PKG_CONFIG").unwrap_or_else(|_| "pkg-config".into()))
        .args(["--static", "--libs", "ffms2", "zimg", "libavfilter", "libavformat", "libavcodec"])
        .env("PKG_CONFIG_PATH", &pc_dir)
        .env("PKG_CONFIG_LIBDIR", &pc_dir)
        .output()
        .expect("failed to run pkg-config");
    assert!(out.status.success(), "pkg-config failed: {}", String::from_utf8_lossy(&out.stderr));
    let flags = String::from_utf8(out.stdout).unwrap();

    let libdir = prefix.join("lib");
    println!("cargo:rustc-link-search=native={}", libdir.display());

    let mut tokens = flags.split_whitespace().peekable();
    let mut seen = std::collections::HashSet::new();
    while let Some(tok) = tokens.next() {
        if let Some(dir) = tok.strip_prefix("-L") {
            println!("cargo:rustc-link-search=native={dir}");
        } else if let Some(lib) = tok.strip_prefix("-l") {
            // C++ runtime is handled per-target below.
            if lib == "stdc++" || lib == "c++" {
                continue;
            }
            if !seen.insert(lib.to_string()) {
                continue;
            }
            let is_ours = ["a", "lib"].iter().any(|ext| {
                libdir.join(format!("lib{lib}.{ext}")).exists() || libdir.join(format!("{lib}.{ext}")).exists()
            });
            // On Windows, link zlib statically too so the exe has no MSYS2 DLL deps.
            if is_ours || (target_os == "windows" && lib == "z") {
                println!("cargo:rustc-link-lib=static={lib}");
            } else {
                println!("cargo:rustc-link-lib={lib}");
            }
        } else if tok == "-framework" {
            if let Some(fw) = tokens.next() {
                println!("cargo:rustc-link-lib=framework={fw}");
            }
        } else if tok == "-pthread" && target_os != "macos" {
            println!("cargo:rustc-link-lib=pthread");
        }
    }

    match target_os {
        "macos" => println!("cargo:rustc-link-lib=c++"),
        "windows" => {
            // mingw-w64: link libstdc++ statically so the binary has no extra DLL deps.
            println!("cargo:rustc-link-lib=static=stdc++");
            for l in ["bcrypt", "ole32", "user32"] {
                println!("cargo:rustc-link-lib={l}");
            }
        }
        _ => println!("cargo:rustc-link-lib=stdc++"),
    }
}

fn bindings(prefix: &PathBuf) {
    let include = prefix.join("include");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", include.display()))
        .clang_arg("-DFFMS_STATIC")
        .allowlist_function("FFMS_.*")
        .allowlist_type("FFMS_.*")
        .allowlist_var("FFMS_.*")
        .allowlist_function("zimg_.*")
        .allowlist_type("zimg_.*")
        .allowlist_var("ZIMG_.*")
        .allowlist_function("av_pix_fmt_desc_get")
        .allowlist_type("AVPixFmtDescriptor")
        .allowlist_var("AV_PIX_FMT_FLAG_.*")
        // libavfilter graph API for IVTC / deinterlacing
        .allowlist_function("av_frame_(alloc|free|get_buffer|unref)")
        .allowlist_function("avfilter_(get_by_name|graph_alloc|graph_free|graph_create_filter|graph_config|link)")
        .allowlist_function("av_buffersrc_add_frame_flags")
        .allowlist_function("av_buffersink_get_frame")
        .allowlist_function("av_strerror")
        // chapters (OP/ED detection)
        .allowlist_function("avformat_(open_input|close_input)")
        .allowlist_function("av_dict_get")
        .allowlist_type("AVFormatContext|AVChapter|AVDictionaryEntry")
        .allowlist_type("AVFrame")
        .allowlist_var("AV_FRAME_FLAG_.*")
        .allowlist_var("AV_BUFFERSRC_FLAG_.*")
        .allowlist_type("AVFilterGraph|AVFilterContext|AVFilter")
        .opaque_type("AVFilterGraph")
        .opaque_type("AVFilterContext")
        .opaque_type("AVFilter")
        .prepend_enum_name(false)
        .default_enum_style(bindgen::EnumVariation::Consts)
        .derive_default(true)
        .layout_tests(true)
        .generate()
        .expect("bindgen failed")
        .write_to_file(out.join("bindings.rs"))
        .expect("write bindings");
}
