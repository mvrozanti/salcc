//! PATH-shim: argv[0] agent detection and link/unlink lifecycle.

use std::ffi::OsStr;

use salcc::shim::{link, shim_agent_name, unlink};

#[test]
fn agent_name_from_arg0() {
    assert_eq!(shim_agent_name(Some(OsStr::new("/usr/bin/claude"))), Some("claude".into()));
    assert_eq!(shim_agent_name(Some(OsStr::new("claude"))), Some("claude".into()));
    assert_eq!(shim_agent_name(Some(OsStr::new("/home/x/.local/bin/codex"))), Some("codex".into()));
    assert_eq!(shim_agent_name(Some(OsStr::new("claude.exe"))), Some("claude".into()));
}

#[test]
fn salcc_itself_is_not_a_shim() {
    assert_eq!(shim_agent_name(Some(OsStr::new("/home/x/.cargo/bin/salcc"))), None);
    assert_eq!(shim_agent_name(Some(OsStr::new("salcc"))), None);
    assert_eq!(shim_agent_name(None), None);
}

// One test owns XDG_BIN_HOME so it never races another thread's env under
// `cargo test`; nextest isolates per process anyway.
#[test]
fn link_lifecycle_and_clobber_safety() {
    let dir = std::env::temp_dir().join(format!("salcc-shim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only env-mutating test in this binary.
    unsafe { std::env::set_var("XDG_BIN_HOME", &dir) };

    link(&["fake-agent".to_string()], false).expect("link");
    let target = dir.join("fake-agent");
    let meta = std::fs::symlink_metadata(&target).expect("shim exists");
    assert!(meta.file_type().is_symlink(), "shim must be a symlink");
    let dest = std::fs::read_link(&target).unwrap().canonicalize().unwrap();
    assert_eq!(dest, std::env::current_exe().unwrap().canonicalize().unwrap());

    link(&["fake-agent".to_string()], false).expect("relink idempotent");
    assert!(std::fs::symlink_metadata(&target).is_ok());

    unlink(&["fake-agent".to_string()]).expect("unlink");
    assert!(std::fs::symlink_metadata(&target).is_err(), "shim removed");

    let occupied = dir.join("busy-agent");
    std::fs::write(&occupied, b"i am a real program").unwrap();
    link(&["busy-agent".to_string()], false).expect("link skips a real file");
    assert_eq!(
        std::fs::read(&occupied).unwrap(),
        b"i am a real program",
        "must not overwrite a non-shim file"
    );

    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::remove_var("XDG_BIN_HOME") };
}
