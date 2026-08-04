//! PATH-shim launch: a symlink named after an agent (e.g. `claude`) that
//! points at the salcc binary. When the OS runs it, salcc sees a non-`salcc`
//! argv[0], resolves the REAL agent binary (skipping its own symlink), and
//! wraps it — so the user types `claude` and gets the styling layer with no
//! shell rc edit, in every shell and every script.
//!
//! Recursion safety, three layers:
//! - real-binary resolution skips any PATH candidate whose canonical path is
//!   this executable (the shim canonicalizes to the salcc binary), so salcc
//!   never spawns itself.
//! - the spawned child gets an absolute path as argv[0], bypassing PATH (and
//!   thus the shim) entirely.
//! - SALCC_ACTIVE (exported to every child) makes a nested shim invocation
//!   exec the real agent directly, never a second proxy.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// If argv[0]'s basename is an agent shim (anything but `salcc`), return it.
pub fn shim_agent_name(arg0: Option<&OsStr>) -> Option<String> {
    let base = Path::new(arg0?).file_name()?.to_str()?;
    let base = base.strip_suffix(".exe").unwrap_or(base);
    if base.is_empty() || base == "salcc" {
        None
    } else {
        Some(base.to_string())
    }
}

/// Entry point when salcc was invoked through an agent shim. Never returns on
/// success (execs or exits).
pub fn run_shim(agent: &str) -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(real) = resolve_real_binary(agent) else {
        anyhow::bail!(
            "salcc shim for `{agent}`: no real `{agent}` found on PATH behind the shim"
        );
    };
    if std::env::var_os("SALCC_ACTIVE").is_some() {
        exec_real(&real, &args);
    }
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(real.to_string_lossy().into_owned());
    argv.extend(args);
    let code = crate::cli::run_proxy(&argv, false)?;
    std::process::exit(code)
}

fn exec_real(real: &Path, args: &[String]) -> ! {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(real).args(args).exec();
    tracing::error!(%err, ?real, "exec of real agent failed");
    std::process::exit(127)
}

/// First executable named `agent` on PATH that is NOT this binary's own
/// symlink (compared by canonical path).
pub fn resolve_real_binary(agent: &str) -> Option<PathBuf> {
    let self_canon = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(agent);
        if !is_executable_file(&candidate) {
            continue;
        }
        if candidate.canonicalize().ok() == self_canon {
            continue;
        }
        return Some(candidate);
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub fn bin_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_BIN_HOME") {
        return Some(PathBuf::from(dir));
    }
    dirs::home_dir().map(|h| h.join(".local/bin"))
}

pub fn link(agents: &[String], force: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let dir = bin_dir().ok_or_else(|| anyhow::anyhow!("no home directory for a bin dir"))?;
    std::fs::create_dir_all(&dir)?;
    for agent in agents {
        let target = dir.join(agent);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if std::fs::read_link(&target)
                    .ok()
                    .and_then(|p| p.canonicalize().ok())
                    == Some(exe.clone())
                {
                    println!("{agent}: already linked");
                    continue;
                }
                if !force {
                    println!(
                        "{agent}: {} is a symlink to something else; rerun with --force to replace",
                        target.display()
                    );
                    continue;
                }
                std::fs::remove_file(&target)?;
            }
            Ok(_) => {
                println!(
                    "{agent}: {} exists and is not a salcc shim; refusing (use --force)",
                    target.display()
                );
                continue;
            }
            Err(_) => {}
        }
        std::os::unix::fs::symlink(&exe, &target)?;
        match resolve_real_binary(agent) {
            Some(real) => println!("{agent}: linked → salcc (wrapping {})", real.display()),
            None => println!(
                "{agent}: linked → salcc, but no real `{agent}` is on PATH yet"
            ),
        }
        warn_if_shadowed(&dir, agent);
    }
    Ok(())
}

pub fn unlink(agents: &[String]) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.canonicalize().ok();
    let dir = bin_dir().ok_or_else(|| anyhow::anyhow!("no home directory for a bin dir"))?;
    for agent in agents {
        let target = dir.join(agent);
        let is_ours = std::fs::symlink_metadata(&target)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
            && std::fs::read_link(&target)
                .ok()
                .and_then(|p| p.canonicalize().ok())
                == exe;
        if is_ours {
            std::fs::remove_file(&target)?;
            println!("{agent}: unlinked");
        } else {
            println!("{agent}: no salcc shim at {}", target.display());
        }
    }
    Ok(())
}

fn warn_if_shadowed(shim_dir: &Path, agent: &str) {
    let Some(path) = std::env::var_os("PATH") else {
        return;
    };
    let shim_canon = shim_dir.canonicalize().ok();
    for dir in std::env::split_paths(&path) {
        if dir.canonicalize().ok() == shim_canon {
            return;
        }
        if is_executable_file(&dir.join(agent)) {
            println!(
                "  warning: {} comes before {} on PATH — the shim will not take effect until {} is earlier (or restart your shell if it was just created)",
                dir.display(),
                shim_dir.display(),
                shim_dir.display()
            );
            return;
        }
    }
}
