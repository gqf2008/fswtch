use std::env;

#[cfg(feature = "bindgen")]
use std::{fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=FREESWITCH_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=FREESWITCH_LIB_DIR");
    println!("cargo:rerun-if-env-changed=FREESWITCH_NO_PKG_CONFIG");
    println!("cargo:rerun-if-changed=build.rs");

    if let Ok(lib_dir) = env::var("FREESWITCH_LIB_DIR") {
        println!("cargo:rustc-link-search=native={lib_dir}");
        println!("cargo:rustc-link-lib=freeswitch");
    } else if env::var_os("FREESWITCH_NO_PKG_CONFIG").is_none() {
        let _ = pkg_config::Config::new().probe("freeswitch");
    }

    generate_bindings();
}

#[cfg(feature = "bindgen")]
fn generate_bindings() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let wrapper = out_dir.join("wrapper.h");
    fs::write(&wrapper, "#include <switch.h>\n").expect("write bindgen wrapper");
    let bundled_config_dir = out_dir.join("bundled-include");

    let mut builder = bindgen::Builder::default()
        .header(wrapper.display().to_string())
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_type("switch_.*")
        .allowlist_function("switch_.*")
        .allowlist_var("SWITCH_.*")
        .rustified_enum("switch_status_t")
        .rustified_enum("switch_module_interface_name_t")
        .rustified_enum("switch_event_types_t")
        .rustified_enum("switch_stack_t")
        .layout_tests(false)
        .derive_default(true);

    if cfg!(feature = "bundled") {
        write_bundled_config_header(&bundled_config_dir);
        builder = builder.clang_arg(format!("-I{}", bundled_config_dir.display()));
        builder = add_include_dirs(builder, bundled_include_dirs());
    } else if let Ok(include_dir) = env::var("FREESWITCH_INCLUDE_DIR") {
        let include_dir = PathBuf::from(include_dir);
        if !include_dir.join("switch_am_config.h").exists() {
            panic!(
                "FREESWITCH_INCLUDE_DIR must point at configured FreeSWITCH headers; missing {}",
                include_dir.join("switch_am_config.h").display()
            );
        }
        builder = builder.clang_arg(format!("-I{}", include_dir.display()));
    } else {
        builder = add_include_dirs(builder, local_workspace_include_dirs());
    }

    let bindings = builder.generate().expect("generate FreeSWITCH bindings");
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("write FreeSWITCH bindings");
}

#[cfg(feature = "bindgen")]
fn add_include_dirs(mut builder: bindgen::Builder, include_dirs: Vec<PathBuf>) -> bindgen::Builder {
    for path in include_dirs {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
            builder = builder.clang_arg(format!("-I{}", path.display()));
        }
    }

    builder
}

#[cfg(all(feature = "bindgen", feature = "bundled"))]
fn bundled_include_dirs() -> Vec<PathBuf> {
    let include_dirs = fswtch_src::include_dirs();
    let switch_header = fswtch_src::root().join("src/include/switch.h");
    if !switch_header.exists() {
        panic!(
            "the bundled feature requires fswtch-src to include FreeSWITCH headers; missing {}",
            switch_header.display()
        );
    }

    include_dirs
}

#[cfg(all(feature = "bindgen", not(feature = "bundled")))]
fn bundled_include_dirs() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(feature = "bindgen")]
fn local_workspace_include_dirs() -> Vec<PathBuf> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/fswtch-sys is nested under the workspace");

    [
        "freeswitch/src/include",
        "freeswitch/libs/apr/include",
        "freeswitch/libs/apr-util/include",
        "freeswitch/libs/libteletone/src",
        "freeswitch/libs/sqlite",
        "freeswitch/libs/pcre2/src",
    ]
    .into_iter()
    .map(|include| repo_root.join(include))
    .collect()
}

#[cfg(feature = "bindgen")]
fn write_bundled_config_header(include_dir: &PathBuf) {
    fs::create_dir_all(include_dir).expect("create bundled include dir");
    fs::write(
        include_dir.join("switch_am_config.h"),
        r#"#ifndef SWITCH_AM_CONFIG_H
#define SWITCH_AM_CONFIG_H

#include <stdint.h>
#include <stddef.h>
#include <inttypes.h>
#include <sys/types.h>

#define SWITCH_INT_16 short
#define SWITCH_INT_32 int
#define SWITCH_INT_64 long
#define SWITCH_SIZE_T size_t
#define SWITCH_SSIZE_T ssize_t

#define SWITCH_SIZEOF_VOIDP __SIZEOF_POINTER__
#define SWITCH_PREFIX_DIR "/usr/local/freeswitch"

#define SWITCH_SIZE_T_FMT "zu"
#define SWITCH_SSIZE_T_FMT "zd"
#define SWITCH_INT64_T_FMT PRId64
#define SWITCH_UINT64_T_FMT PRIu64

#endif
"#,
    )
    .expect("write bundled switch_am_config.h");
}

#[cfg(not(feature = "bindgen"))]
fn generate_bindings() {}
