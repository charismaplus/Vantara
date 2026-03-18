use std::{env, fs, path::PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    println!("cargo:rerun-if-env-changed=WORKSPACE_TERMINAL_EMBED_TMUX_PATH");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is not set"));
    let generated_path = out_dir.join("embedded_tmux.rs");
    let generated_source = match env::var("WORKSPACE_TERMINAL_EMBED_TMUX_PATH") {
        Ok(path) if !path.trim().is_empty() => build_embedded_tmux_source(&path),
        _ => "pub const EMBEDDED_TMUX_BYTES: Option<&[u8]> = None;\n\
             pub const EMBEDDED_TMUX_HASH: Option<&str> = None;\n"
            .to_string(),
    };

    fs::write(&generated_path, generated_source).expect("failed to write embedded_tmux.rs");
    tauri_build::build()
}

fn build_embedded_tmux_source(path: &str) -> String {
    let canonical = PathBuf::from(path)
        .canonicalize()
        .unwrap_or_else(|err| panic!("failed to resolve tmux shim binary at {path}: {err}"));
    println!("cargo:rerun-if-changed={}", canonical.display());

    let bytes = fs::read(&canonical).unwrap_or_else(|err| {
        panic!(
            "failed to read tmux shim binary at {}: {err}",
            canonical.display()
        )
    });
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let hash = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let include_path = normalize_include_path(&canonical);
    format!(
        "pub const EMBEDDED_TMUX_BYTES: Option<&[u8]> = Some(include_bytes!(r#\"{include_path}\"#) as &[u8]);\n\
         pub const EMBEDDED_TMUX_HASH: Option<&str> = Some(\"{hash}\");\n"
    )
}

fn normalize_include_path(path: &PathBuf) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    if let Some(stripped) = normalized.strip_prefix("//?/") {
        normalized = stripped.to_string();
    }
    normalized
}
