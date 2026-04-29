use std::path::PathBuf;

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("freeswitch")
}

pub fn include_dirs() -> Vec<PathBuf> {
    let root = root();
    ["src/include", "libs/apr/include", "libs/libteletone/src"]
        .into_iter()
        .map(|relative| root.join(relative))
        .collect()
}
