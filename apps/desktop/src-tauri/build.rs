use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    // Declaring the app's commands makes each one require an explicit permission
    // in `capabilities/`, instead of being callable from any window by default.
    let manifest = tauri_build::AppManifest::new().commands(&[
        "protocol_info",
        "app_version",
        "watchdog_request",
        "update_tray",
        "save_log_export",
        "save_settings_export",
        "mcp_version",
        "mcp_clients",
        "set_up_mcp_client",
        "remove_mcp_client",
        "system_languages",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .expect("failed to run tauri-build");
    languages();
}

/// Lists the GUI's language files (`src/lang/<tag>.json`) for `src/lang.rs`
/// as `(tag, include_str!(path))` pairs, sorted by tag, so the tray speaks
/// every language the window does without a code change. Only the folder's
/// own `.json` files are listed, by their fixed location, except
/// `languages.json`, the window's list of them (its tests check that the
/// list and the files agree); nothing here reads a path from anywhere else.
fn languages() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let dir = Path::new(&manifest).join("..").join("src").join("lang");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<(String, PathBuf)> = fs::read_dir(&dir)
        .expect("the language files' folder")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let tag = path
                .file_name()?
                .to_str()?
                .strip_suffix(".json")?
                .to_owned();
            (path.is_file() && tag != "languages").then_some((tag, path))
        })
        .collect();
    files.sort();
    let mut out = String::from("&[\n");
    for (tag, path) in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let path = path.canonicalize().expect("a language file's path");
        out.push_str(&format!(
            "    ({tag:?}, include_str!({:?})),\n",
            path.display().to_string()
        ));
    }
    out.push_str("]\n");
    let target = Path::new(&env::var("OUT_DIR").expect("OUT_DIR")).join("languages.rs");
    fs::write(target, out).expect("languages.rs");
}
