use std::{env, fs, path::PathBuf};

fn main() {
    // Each board (src/board.rs) has its own flash layout. None is called memory.x in this
    // directory: the linker would find that one before the copy in OUT_DIR.
    let memory = if env::var_os("CARGO_FEATURE_NICENANO").is_some() { "memory-nicenano.x" } else { "memory-pca10059.x" };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy(memory, out.join("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory-pca10059.x");
    println!("cargo:rerun-if-changed=memory-nicenano.x");
}
