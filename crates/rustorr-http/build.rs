//! Embeds the built web interface (`web/dist`, or `RUSTORR_WEB_DIST`) into
//! the binary. Without a build the table is empty and `/` answers with a
//! placeholder, so the Rust crates build and test without Node.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    println!("cargo:rerun-if-env-changed=RUSTORR_WEB_DIST");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let dist = env::var_os("RUSTORR_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../web/dist"));
    println!("cargo:rerun-if-changed={}", dist.display());

    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &mut files);
    }
    files.sort();
    for (_, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut code =
        String::from("/// Files of the built interface: (URL path, absolute path on disk).\n");
    code.push_str("pub(crate) static FILES: &[(&str, &[u8])] = &[\n");
    for (url, path) in &files {
        code.push_str(&format!(
            "    ({url:?}, include_bytes!({:?})),\n",
            path.canonicalize().expect("a listed file exists")
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets it"));
    fs::write(out.join("web_assets.rs"), code).expect("OUT_DIR is writable");
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            collect(root, &path, files);
        } else if let Ok(relative) = path.strip_prefix(root) {
            let url = format!("/{}", relative.to_string_lossy().replace('\\', "/"));
            files.push((url, path));
        }
    }
}
