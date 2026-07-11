use std::{env, error::Error, fs, io, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=FREESWITCH_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=FREESWITCH_LIB_DIR");
    println!("cargo:rerun-if-env-changed=FREESWITCH_NO_PKG_CONFIG");
    println!("cargo:rerun-if-changed=build.rs");

    if let Ok(lib_dir) = env::var("FREESWITCH_LIB_DIR") {
        println!("cargo:rustc-link-search=native={lib_dir}");
        println!("cargo:rustc-link-lib=freeswitch");
    } else if env::var_os("FREESWITCH_NO_PKG_CONFIG").is_none()
        && let Err(error) = pkg_config::Config::new().probe("freeswitch")
    {
        println!("cargo:warning=pkg-config could not find freeswitch: {error}");
    }

    generate_bindings()?;
    Ok(())
}

fn generate_bindings() -> Result<(), Box<dyn Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let wrapper = out_dir.join("wrapper.h");
    fs::write(
        &wrapper,
        r#"#include <switch.h>
#if __has_include(<speex/speex_echo.h>)
#include <speex/speex_echo.h>
#include <speex/speex_preprocess.h>
#include <speex/speex_resampler.h>
#endif
"#,
    )?;
    let bundled_config_dir = out_dir.join("bundled-include");

    let mut builder = bindgen::Builder::default()
        .header(wrapper.display().to_string())
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_type("switch_.*")
        .allowlist_function("switch_.*")
        .allowlist_var("SWITCH_.*")
        .allowlist_function("speex_.*")
        .allowlist_type("Speex.*")
        .allowlist_type("spx_.*")
        .allowlist_var("SPEEX_.*")
        .rustified_enum("switch_status_t")
        .rustified_enum("switch_module_interface_name_t")
        .rustified_enum("switch_event_types_t")
        .rustified_enum("switch_stack_t")
        .layout_tests(false)
        .generate_comments(false)
        .derive_default(true);

    if cfg!(feature = "bundled") {
        write_bundled_config_header(&bundled_config_dir)?;
        builder = builder.clang_arg(format!("-I{}", bundled_config_dir.display()));
        builder = add_include_dirs(builder, bundled_include_dirs()?);
    } else if let Ok(include_dir) = env::var("FREESWITCH_INCLUDE_DIR") {
        let include_dir = PathBuf::from(include_dir);
        let config_header = include_dir.join("switch_am_config.h");
        if !config_header.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "FREESWITCH_INCLUDE_DIR must point at configured FreeSWITCH headers; missing {}",
                    config_header.display()
                )
            )
            .into());
        }
        builder = builder.clang_arg(format!("-I{}", include_dir.display()));
    } else {
        builder = add_include_dirs(builder, local_workspace_include_dirs()?);
    }

    // Add common system include paths so bindgen can find optional headers like speexdsp
    // (macOS homebrew: /opt/homebrew/include; Linux: /usr/include; older macOS: /usr/local/include).
    for system_inc in ["/opt/homebrew/include", "/usr/local/include", "/usr/include"] {
        let path = PathBuf::from(system_inc);
        if path.exists() {
            builder = builder.clang_arg(format!("-I{system_inc}"));
        }
    }

    let bindings = builder.generate()?;
    bindings.write_to_file(out_dir.join("bindings.rs"))?;
    Ok(())
}

fn add_include_dirs(mut builder: bindgen::Builder, include_dirs: Vec<PathBuf>) -> bindgen::Builder {
    for path in include_dirs {
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
            builder = builder.clang_arg(format!("-I{}", path.display()));
        }
    }

    builder
}

#[cfg(feature = "bundled")]
fn bundled_include_dirs() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let include_dirs = fswtch_src::include_dirs();
    let switch_header = fswtch_src::root().join("src/include/switch.h");
    if !switch_header.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the bundled feature requires fswtch-src to include FreeSWITCH headers; missing {}",
                switch_header.display()
            ),
        )
        .into());
    }

    Ok(include_dirs)
}

#[cfg(not(feature = "bundled"))]
fn bundled_include_dirs() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    Ok(Vec::new())
}

fn local_workspace_include_dirs() -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "crates/fswtch-sys must be nested under the workspace",
            )
        })?;

    Ok([
        "freeswitch/src/include",
        "freeswitch/libs/apr/include",
        "freeswitch/libs/apr-util/include",
        "freeswitch/libs/libteletone/src",
        "freeswitch/libs/sqlite",
        "freeswitch/libs/pcre2/src",
    ]
    .into_iter()
    .map(|include| repo_root.join(include))
    .collect())
}

fn write_bundled_config_header(include_dir: &PathBuf) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(include_dir)?;
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
    )?;
    Ok(())
}
