//! Integration test #1 (MILESTONE-5 acceptance #3).
//!
//! - spawn `cmd /c echo hello` (Windows) / `sh -c "echo hello"` (Unix),
//! - assert the output bytes arrive,
//! - resize to 80x24,
//! - spawn a long-running process, kill it, and assert the process is dead.

use std::time::Duration;

use bebok_pty::{CommandSpec, PtyManager, SpawnOptions};

#[cfg(windows)]
fn echo_command() -> CommandSpec {
    CommandSpec {
        program: "cmd".to_string(),
        args: vec!["/c".to_string(), "echo hello".to_string()],
    }
}

#[cfg(not(windows))]
fn echo_command() -> CommandSpec {
    CommandSpec {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), "echo hello".to_string()],
    }
}

/// `echo hello` after a short delay, so a client can connect before the output
/// is produced (exercises the live fan-out, not just the scrollback).
#[cfg(windows)]
fn streamed_echo_command() -> CommandSpec {
    CommandSpec {
        program: "cmd".to_string(),
        args: vec!["/c".to_string(), "timeout /t 1 /nobreak >nul & echo hello".to_string()],
    }
}

#[cfg(not(windows))]
fn streamed_echo_command() -> CommandSpec {
    CommandSpec {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), "sleep 0.2; echo hello".to_string()],
    }
}

#[cfg(windows)]
fn sleep_command() -> CommandSpec {
    CommandSpec {
        program: "cmd".to_string(),
        args: vec!["/c".to_string(), "timeout /t 60 /nobreak".to_string()],
    }
}

#[cfg(not(windows))]
fn sleep_command() -> CommandSpec {
    CommandSpec {
        program: "sh".to_string(),
        args: vec!["-c".to_string(), "sleep 60".to_string()],
    }
}

/// Drain the client's live channel until `needle` appears, the child exits, or
/// the timeout elapses. Returns the accumulated bytes.
async fn read_until(client: &mut bebok_pty::PtyClient, needle: &str, timeout: Duration) -> Vec<u8> {
    let mut output = Vec::new();
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => break,
            chunk = client.recv() => match chunk {
                Some(bytes) => output.extend_from_slice(&bytes),
                None => break, // child exited (pty closed)
            },
        }
        if String::from_utf8_lossy(&output).contains(needle) {
            break;
        }
    }
    output
}

#[tokio::test]
async fn echo_resize_and_kill_process_tree() {
    let manager = PtyManager::new();

    // 1. Spawn `echo hello` and assert the bytes are delivered. The command is
    //    delayed so the client is connected before the output is produced, which
    //    exercises the live fan-out (acceptance: "assert broadcast bytes").
    let session = manager
        .spawn(SpawnOptions {
            command: Some(streamed_echo_command()),
            ..Default::default()
        })
        .expect("spawn echo pty");

    let mut client = session.connect();
    let output = read_until(&mut client, "hello", Duration::from_secs(10)).await;
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("hello"),
        "expected 'hello' in pty output, got: {text:?}"
    );

    // 2. Resize to 80x24 (must not error).
    session.resize(24, 80).expect("resize to 80x24");

    // 3. Spawn a long-running process, kill it, assert it is dead.
    let sleeper = manager
        .spawn(SpawnOptions {
            command: Some(sleep_command()),
            ..Default::default()
        })
        .expect("spawn sleeper pty");

    let _pid = sleeper.process_id().expect("sleeper has a pid");
    sleeper.kill().expect("kill sleeper");

    // The reader detects EOF, reaps the child and marks the session exited.
    let mut exited = false;
    for _ in 0..100 {
        if sleeper.is_exited() {
            exited = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(exited, "sleeper session must report exited after kill");

    // On Unix we can also verify the OS process itself is gone.
    #[cfg(not(windows))]
    {
        let proc_path = std::path::PathBuf::from(format!("/proc/{_pid}"));
        assert!(
            !proc_path.exists(),
            "process {_pid} must be dead after kill (tree kill)"
        );
    }
}

#[tokio::test]
async fn reattach_replays_scrollback() {
    let manager = PtyManager::new();
    let session = manager
        .spawn(SpawnOptions {
            command: Some(echo_command()),
            ..Default::default()
        })
        .expect("spawn echo pty");

    // Wait for the command to finish so its output has been drained into the
    // scrollback (the engine-side history that survives a client disconnect).
    for _ in 0..200 {
        if session.is_exited() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(session.is_exited(), "echo session should have exited");

    // A reconnecting client gets the history from the scrollback snapshot.
    let second = session.connect();
    let replay = String::from_utf8_lossy(&second.scrollback);
    assert!(
        replay.contains("hello"),
        "scrollback replay must contain 'hello', got: {replay:?}"
    );
}
