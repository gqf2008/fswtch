use std::{
    env,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let cpp_dir = manifest_dir.join("cpp");
    let wrapper_header = cpp_dir.join("wrapper").join("fswtch_apm.h");

    println!("cargo:rerun-if-changed=build.rs");
    // Watch the whole vendored C++ tree so any source/header added during closure convergence
    // (Phase 2+) re-runs the build without having to enumerate files by hand.
    rerun_for_dir(&cpp_dir);

    // Build the vendored C++ (AEC3 + its transitive closure) as a static library via CMake.
    let dst = cmake::Config::new(&cpp_dir).profile("Release").build();

    println!("cargo:rustc-link-search=native={}", dst.display());
    println!("cargo:rustc-link-lib=static=fswtch_apm");

    // The static lib contains C++ objects; propagate the C++ runtime to the final link.
    // macOS uses libc++ (`-lc++`); other Unix uses libstdc++ (`-lstdc++`).
    if !cfg!(target_env = "msvc") {
        let cppstd = if cfg!(target_os = "macos") {
            "c++"
        } else {
            "stdc++"
        };
        println!("cargo:rustc-link-lib=dylib={cppstd}");
    }

    generate_bindings(&wrapper_header);
}

fn rerun_for_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rerun_for_dir(&path);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn generate_bindings(wrapper_header: &Path) {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let bindings = bindgen::Builder::default()
        .header(wrapper_header.display().to_string())
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_function("fswtch_.*")
        .allowlist_type("fswtch_.*")
        .layout_tests(false)
        .generate_comments(false)
        .derive_default(true)
        .generate()
        .expect("unable to generate AEC3 C API bindings");
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("unable to write bindings.rs");
}
