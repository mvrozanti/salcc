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

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

fn dir_index(dirs: &[PathBuf], target: &Path) -> Option<usize> {
    let target = target.canonicalize().ok()?;
    dirs.iter().position(|d| d.canonicalize().ok().as_deref() == Some(&target))
}

fn is_writable_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(format!(".salcc-write-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Where a shim for `agent` must live to actually take effect: a writable
/// PATH dir ahead of the real binary. Prefers the user bin dir when it
/// qualifies; falls back to it (shadowed) when nothing ahead is writable.
fn choose_shim_dir(agent: &str) -> anyhow::Result<(PathBuf, Option<PathBuf>, bool)> {
    let real = resolve_real_binary(agent);
    let dirs = path_dirs();
    let real_idx = real
        .as_deref()
        .and_then(Path::parent)
        .and_then(|p| dir_index(&dirs, p))
        .unwrap_or(usize::MAX);
    let preferred = bin_dir();
    if real_idx == usize::MAX {
        let pref =
            preferred.ok_or_else(|| anyhow::anyhow!("no home directory for a bin dir"))?;
        std::fs::create_dir_all(&pref)?;
        return Ok((pref, real, false));
    }
    if let Some(pref) = &preferred {
        let pidx = dir_index(&dirs, pref).unwrap_or(usize::MAX);
        let _ = std::fs::create_dir_all(pref);
        if pidx < real_idx && is_writable_dir(pref) {
            return Ok((pref.clone(), real, false));
        }
    }
    for (idx, dir) in dirs.iter().enumerate() {
        if idx >= real_idx {
            break;
        }
        if is_writable_dir(dir) {
            return Ok((dir.clone(), real, false));
        }
    }
    let fallback = preferred.ok_or_else(|| anyhow::anyhow!("no home directory for a bin dir"))?;
    std::fs::create_dir_all(&fallback)?;
    Ok((fallback, real, true))
}

pub fn link(agents: &[String], force: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    for agent in agents {
        let (dir, real, shadowed) = choose_shim_dir(agent)?;
        let target = dir.join(agent);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if std::fs::read_link(&target)
                    .ok()
                    .and_then(|p| p.canonicalize().ok())
                    == Some(exe.clone())
                {
                    println!("{agent}: already linked ({})", target.display());
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
        match &real {
            Some(real) => println!("{agent}: linked {} → salcc (wrapping {})", target.display(), real.display()),
            None => println!("{agent}: linked {} → salcc, but no real `{agent}` is on PATH yet", target.display()),
        }
        if shadowed {
            println!(
                "  warning: {} is not on PATH ahead of the real `{agent}`; the shim will not take effect until an earlier PATH dir is writable or you add {} ahead",
                dir.display(),
                dir.display()
            );
        }
    }
    Ok(())
}

fn shim_search_dirs() -> Vec<PathBuf> {
    let mut dirs = bin_dir().into_iter().collect::<Vec<_>>();
    dirs.extend(path_dirs());
    dirs
}

/// Path of an installed salcc shim for `agent`, searching every candidate dir.
pub fn installed_shim(agent: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok())?;
    for dir in shim_search_dirs() {
        let target = dir.join(agent);
        let is_ours = std::fs::symlink_metadata(&target)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
            && std::fs::read_link(&target).ok().and_then(|p| p.canonicalize().ok()) == Some(exe.clone());
        if is_ours {
            return Some(target);
        }
    }
    None
}

pub fn unlink(agents: &[String]) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.canonicalize().ok();
    let dirs = shim_search_dirs();
    for agent in agents {
        let mut removed = false;
        for dir in &dirs {
            let target = dir.join(agent);
            let is_ours = std::fs::symlink_metadata(&target)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
                && std::fs::read_link(&target).ok().and_then(|p| p.canonicalize().ok()) == exe;
            if is_ours {
                std::fs::remove_file(&target)?;
                println!("{agent}: unlinked {}", target.display());
                removed = true;
            }
        }
        if !removed {
            println!("{agent}: no salcc shim found");
        }
    }
    Ok(())
}

