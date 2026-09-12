use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=models.dev.json");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    fs::copy("models.dev.json", out.join("models.dev.json"))
        .expect("copy vendored models.dev snapshot");
}
