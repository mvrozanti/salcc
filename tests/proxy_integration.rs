//! Integration tests hosting salcc inside a real test pty: passthrough
//! output, exit-code propagation, resize propagation and stability, and piped
//! non-tty byte identity.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use portable_pty::{MasterPty, PtySize, native_pty_system};

const WAIT_BUDGET: Duration = Duration::from_secs(10);

struct PtyRun {
    output: Vec<u8>,
    exit_code: u32,
}

fn run_salcc_in_pty<F>(args: &[&str], drive: F) -> PtyRun
where
    F: FnOnce(&mut (dyn Write + Send), &dyn MasterPty),
{
    let pair = native_pty_system().openpty(PtySize::default()).unwrap();
    let mut cmd = portable_pty::CommandBuilder::new(env!("CARGO_BIN_EXE_salcc"));
    cmd.args(args);
    cmd.cwd(std::env::current_dir().unwrap());
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        let mut collected = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => collected.extend_from_slice(&buf[..n]),
            }
        }
        out_tx.send(collected).ok();
    });
    drive(&mut writer, pair.master.as_ref());
    drop(writer);
    let (status_tx, status_rx) = mpsc::channel();
    thread::spawn(move || {
        status_tx.send(child.wait()).ok();
    });
    let status = status_rx
        .recv_timeout(WAIT_BUDGET)
        .expect("salcc did not exit within budget")
        .expect("wait on salcc failed");
    drop(pair.master);
    let output = out_rx
        .recv_timeout(WAIT_BUDGET)
        .expect("output collector hung");
    PtyRun {
        output,
        exit_code: status.exit_code(),
    }
}

#[test]
fn passthrough_emits_child_output() {
    let run = run_salcc_in_pty(&["printf", "hello\\r\\n"], |_, _| {});
    let text = String::from_utf8_lossy(&run.output);
    assert!(text.contains("hello"), "output was: {text:?}");
    assert_eq!(run.exit_code, 0);
}

#[test]
fn exit_code_propagates_exactly() {
    let run = run_salcc_in_pty(&["sh", "-c", "exit 42"], |_, _| {});
    assert_eq!(run.exit_code, 42);
}

#[test]
fn resize_during_cat_stays_clean() {
    let run = run_salcc_in_pty(&["cat"], |writer, master| {
        writer.write_all(b"before-resize\r").unwrap();
        writer.flush().unwrap();
        thread::sleep(Duration::from_millis(200));
        master
            .resize(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        thread::sleep(Duration::from_millis(200));
        writer.write_all(b"after-resize\r").unwrap();
        writer.flush().unwrap();
        thread::sleep(Duration::from_millis(200));
        writer.write_all(&[0x04]).unwrap();
        writer.flush().unwrap();
    });
    let text = String::from_utf8_lossy(&run.output);
    assert!(text.contains("before-resize"), "output was: {text:?}");
    assert!(text.contains("after-resize"), "output was: {text:?}");
    assert_eq!(run.exit_code, 0);
}

#[test]
fn resize_propagates_to_child_pty() {
    let run = run_salcc_in_pty(&["sh", "-c", "sleep 1; stty size"], |_, master| {
        thread::sleep(Duration::from_millis(300));
        master
            .resize(PtySize {
                rows: 33,
                cols: 117,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
    });
    let text = String::from_utf8_lossy(&run.output);
    assert!(text.contains("33 117"), "output was: {text:?}");
    assert_eq!(run.exit_code, 0);
}

#[test]
fn piped_mode_is_byte_identical() {
    let path = std::env::temp_dir().join(format!("salcc_identity_{}.bin", std::process::id()));
    let mut data = vec![0u8; 1_000_000];
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    for byte in &mut data {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *byte = (state >> 56) as u8;
    }
    std::fs::write(&path, &data).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_salcc"))
        .arg("cat")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    std::fs::remove_file(&path).ok();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, data, "piped output diverged from input bytes");
}
