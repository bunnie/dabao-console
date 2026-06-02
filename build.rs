// build.rs - standalone equivalent of `cargo xtask install-toolkit`
//
// Runs automatically when this crate is built. It ensures that:
//   1. The custom riscv32imac-unknown-xous-elf sysroot (with version-matching) is present.
//   2. The bare-metal riscv32imac-unknown-none-elf target is present (via rustup).
//
// The logic is a direct port of `ensure_compiler` + `ensure_kernel_compiler` from
// the xtask utils.rs, with the following adaptations:
//
//   * `force_install = true` is hard-coded (build scripts are non-interactive).
//   * `remove_existing = false` is hard-coded (no --force flag equivalent here; add a `REINSTALL_TOOLKIT=1`
//     env var override if you need that behavior).
//   * `atty` / interactive prompts are removed - a build script always installs.
//   * `lazy_static` is replaced with plain `const` - no runtime dependency needed.
//   * `project_root()` is replaced with `CARGO_MANIFEST_DIR` (provided by Cargo).
//   * `println!` output uses `cargo:warning=` so it surfaces during `cargo build`.
//   * HTTP is handled by `ureq` (pure Rust, TLS included) - no curl/wget needed.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Read},
    path::PathBuf,
    process::{Command, Stdio},
};

// ---------------------------------------------------------------------------
// Target triples (mirrored from main.rs)
// ---------------------------------------------------------------------------

const TARGET_TRIPLE_RISCV32: &str = "riscv32imac-unknown-xous-elf";
const TARGET_TRIPLE_RISCV32_KERNEL: &str = "riscv32imac-unknown-none-elf";

// ---------------------------------------------------------------------------
// Toolchain release endpoints (mirrored from utils.rs)
// ---------------------------------------------------------------------------

const TOOLCHAIN_RELEASE_URL_RISCV32: &str = "https://api.github.com/repos/betrusted-io/rust/releases";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    // Re-run only when this script itself changes, not on every source edit.
    println!("cargo:rerun-if-changed=build.rs");

    // Optional escape hatch: set SKIP_TOOLKIT_INSTALL=1 to bypass entirely.
    if std::env::var("SKIP_TOOLKIT_INSTALL").as_deref() == Ok("1") {
        cargo_warn("SKIP_TOOLKIT_INSTALL is set - skipping toolkit check.");
        return;
    }

    // Determine whether to forcibly reinstall (analogous to `--force`).
    let force_reinstall = std::env::var("REINSTALL_TOOLKIT").as_deref() == Ok("1");

    // 1. Ensure the custom Xous sysroot (with version check).
    if let Err(e) = ensure_compiler(TARGET_TRIPLE_RISCV32, force_reinstall) {
        panic!("install-toolkit: ensure_compiler failed: {}", e);
    }

    // 2. Ensure the bare-metal kernel target (no version check needed).
    if let Err(e) = ensure_kernel_compiler(TARGET_TRIPLE_RISCV32_KERNEL) {
        panic!("install-toolkit: ensure_kernel_compiler failed: {}", e);
    }
}

// ---------------------------------------------------------------------------
// ensure_compiler
//
// Port of `ensure_compiler(target, force_install=true, remove_existing)`.
// Downloads and installs the custom Xous toolchain zip if needed.
// ---------------------------------------------------------------------------

fn ensure_compiler(target: &str, remove_existing: bool) -> Result<(), String> {
    if let Some(sysroot_path) = get_sysroot(Some(target), /* check_version= */ true)? {
        if remove_existing {
            cargo_warn(&format!("Target path exists, removing it for reinstall: {}", sysroot_path));
            let mut version_path = PathBuf::from(&sysroot_path);
            version_path.push("lib");
            version_path.push("rustlib");
            version_path.push(target);
            std::fs::remove_dir_all(&version_path)
                .map_err(|e| format!("unable to remove existing toolchain: {}", e))?;

            // Also clear the target directories so nothing stale lingers.
            let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
            let workspace_root = PathBuf::from(&manifest_dir);

            let target_main = workspace_root.join("target").join(target);
            std::fs::remove_dir_all(&target_main).ok();

            let target_loader = workspace_root.join("loader").join("target").join(target);
            std::fs::remove_dir_all(&target_loader).ok();
        } else {
            // Sysroot present and version matches - nothing to do.
            cargo_warn(&format!("Toolchain for {} is already installed and up-to-date.", target));
            return Ok(());
        }
    }

    // No suitable sysroot found - we need to download a new one.
    let toolchain_path = get_sysroot(None, /* check_version= */ false)?
        .ok_or_else(|| "default toolchain not installed".to_owned())?;

    // Determine the current rustc version to pick a matching toolchain zip.
    let ver = rustc_version()?;
    cargo_warn(&format!(
        "Installing toolchain for {} into {} (rustc {}.{}.{})",
        target, toolchain_path, ver.0, ver.1, ver.2
    ));

    let toolchain_url = get_toolchain_url(target, ver.0, ver.1, ver.2)?;
    cargo_warn(&format!("Downloading toolchain from {}...", toolchain_url));

    let zip_data = http_get(&toolchain_url)?;
    cargo_warn(&format!("Download successful ({} bytes). Extracting...", zip_data.len()));

    extract_zip(Cursor::new(zip_data), &toolchain_path)?;
    cargo_warn("Toolchain successfully installed.");
    Ok(())
}

// ---------------------------------------------------------------------------
// ensure_kernel_compiler
//
// Port of `ensure_kernel_compiler(target, force_install=true)`.
// Uses `rustup target add` - no custom zip needed for the bare-metal target.
// ---------------------------------------------------------------------------

fn ensure_kernel_compiler(target: &str) -> Result<(), String> {
    // If the sysroot already exists (no version check required), we're done.
    if get_sysroot(Some(target), /* check_version= */ false)?.is_some() {
        cargo_warn(&format!("Kernel toolchain target {} is already installed.", target));
        return Ok(());
    }

    cargo_warn(&format!("Installing kernel toolchain target {} via rustup...", target));

    let output = Command::new("rustup")
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .args(["target", "add", target])
        .spawn()
        .map_err(|e| format!("could not run rustup: {}", e))?
        .wait_with_output()
        .map_err(|e| format!("rustup wait failed: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "rustup target add {} failed: {}",
            target,
            String::from_utf8_lossy(&output.stdout)
        ));
    }

    cargo_warn(&format!("Kernel toolchain target {} installed.", target));
    Ok(())
}

// ---------------------------------------------------------------------------
// get_sysroot
//
// Port of `get_sysroot(target, check_version)`.
// Returns the sysroot path string when the toolchain is present and (if
// check_version is true) its RUST_VERSION file matches the running rustc.
// ---------------------------------------------------------------------------

fn get_sysroot(target: Option<&str>, check_version: bool) -> Result<Option<String>, String> {
    let mut args = vec!["--print", "sysroot"];
    if let Some(t) = target {
        args.push("--target");
        args.push(t);
    }

    let output = Command::new("rustc")
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .args(&args)
        .spawn()
        .expect("could not run rustc")
        .wait_with_output()
        .map_err(|e| format!("rustc sysroot check failed: {}", e))?;

    if !output.status.success() {
        return Ok(None);
    }

    let sysroot = String::from_utf8(output.stdout)
        .map_err(|_| "Unable to read rustc sysroot output".to_owned())?
        .trim()
        .to_owned();

    if let Some(t) = target {
        let base = PathBuf::from(&sysroot).join("lib").join("rustlib").join(t);

        if check_version {
            let version_file = base.join("RUST_VERSION");
            match File::open(&version_file) {
                Ok(mut f) => {
                    let mut installed_ver = String::new();
                    f.read_to_string(&mut installed_ver)
                        .map_err(|_| "Unable to read RUST_VERSION".to_owned())?;

                    let running_ver = format!("{}", rustc_version_string()?);
                    if installed_ver.trim() != running_ver.trim() {
                        cargo_warn(&format!(
                            "Toolchain version mismatch: running {}, installed {}. Reinstalling.",
                            running_ver.trim(),
                            installed_ver.trim()
                        ));
                        return Ok(None);
                    }
                }
                Err(_) => {
                    cargo_warn("Outdated toolchain installed (no RUST_VERSION file). Reinstalling.");
                    return Ok(None);
                }
            }
        } else if !base.exists() {
            return Ok(None);
        }
    }

    Ok(Some(sysroot))
}

// ---------------------------------------------------------------------------
// get_toolchain_url
//
// Port of the inner `get_toolchain_url` closure from utils.rs.
// Queries the GitHub releases API and returns the best-matching zip URL.
// ---------------------------------------------------------------------------

fn get_toolchain_url(target: &str, major: u64, minor: u64, patch: u64) -> Result<String, String> {
    let url = match target {
        TARGET_TRIPLE_RISCV32 => TOOLCHAIN_RELEASE_URL_RISCV32,
        other => return Err(format!("No toolchain release URL configured for target {}", other)),
    };

    let body = http_get(url)?;
    let text = String::from_utf8(body).map_err(|e| format!("non-UTF-8 release JSON: {}", e))?;
    let j: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("JSON parse error: {}", e))?;

    let releases = j.as_array().ok_or("GitHub response is not an array")?;
    let target_prefix = format!("{}.{}.{}", major, minor, patch);
    let mut tag_urls: BTreeMap<String, String> = BTreeMap::new();

    for r in releases {
        let keys = match r.as_object() {
            Some(k) => k,
            None => continue,
        };
        let tag = match keys.get("tag_name").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };
        if !tag.starts_with(&target_prefix) {
            continue;
        }
        let assets = match keys.get("assets").and_then(|v| v.as_array()) {
            Some(a) => a,
            None => continue,
        };

        let matching = assets.iter().find(|asset| {
            asset
                .get("name")
                .and_then(|n| n.as_str())
                .map(|name| name.split('_').next().map(|prefix| target.starts_with(prefix)).unwrap_or(false))
                .unwrap_or(false)
        });

        if let Some(asset) = matching {
            if let Some(url) = asset.get("browser_download_url").and_then(|u| u.as_str()) {
                tag_urls.insert(tag.to_owned(), url.to_owned());
            }
        }
    }

    tag_urls
        .into_iter()
        .last()
        .map(|(_, url)| url)
        .ok_or_else(|| format!("No toolchain release found for Rust {}", target_prefix))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Synchronous HTTP GET using `ureq`. Follows redirects automatically (GitHub
/// release asset URLs redirect to S3) and returns the full response body.
fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let mut body: Vec<u8> = Vec::new();
    ureq::get(url)
        .call()
        .map_err(|e| format!("http_get '{}': {}", url, e))?
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("http_get read '{}': {}", url, e))?;
    Ok(body)
}

/// Extract a zip archive into `extract_to`, mirroring `extract_zip` in utils.rs.
fn extract_zip<R: Read + std::io::Seek, P: AsRef<std::path::Path>>(
    archive_data: R,
    extract_to: P,
) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(archive_data).map_err(|e| format!("unable to open zip: {}", e))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("unable to read zip entry {}: {}", i, e))?;
        let out_path = extract_to.as_ref().join(entry.mangled_name());

        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| format!("mkdir {}: {}", out_path.display(), e))?;
        } else {
            if let Some(parent) = out_path.parent() {
                if !parent.exists() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
                }
            }
            let mut outfile =
                File::create(&out_path).map_err(|e| format!("create {}: {}", out_path.display(), e))?;
            std::io::copy(&mut entry, &mut outfile)
                .map_err(|e| format!("write {}: {}", out_path.display(), e))?;
        }
    }
    Ok(())
}

/// Returns `(major, minor, patch)` for the running rustc.
fn rustc_version() -> Result<(u64, u64, u64), String> {
    let v = rustc_version_string()?;
    // v is something like "1.78.0"
    let parts: Vec<u64> = v.split('.').map(|s| s.parse::<u64>().unwrap_or(0)).collect();
    if parts.len() < 3 {
        return Err(format!("Could not parse rustc version '{}'", v));
    }
    Ok((parts[0], parts[1], parts[2]))
}

/// Returns the raw version string from `rustc --version`.
fn rustc_version_string() -> Result<String, String> {
    let out = Command::new("rustc")
        .args(["--version"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("could not run rustc")
        .wait_with_output()
        .map_err(|e| format!("rustc --version failed: {}", e))?;

    let raw = String::from_utf8(out.stdout).map_err(|_| "non-UTF-8 rustc output".to_owned())?;
    // "rustc 1.78.0 (9b00956e5 2024-04-29)" → "1.78.0"
    raw.split_whitespace()
        .nth(1)
        .map(|s| s.to_owned())
        .ok_or_else(|| format!("unexpected rustc --version output: {}", raw.trim()))
}

/// Emit a build-script warning so it surfaces in `cargo build` output.
fn cargo_warn(msg: &str) {
    println!("cargo:warning=[toolkit] {}", msg);
}
