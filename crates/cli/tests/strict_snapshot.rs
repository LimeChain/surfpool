use std::{
    fs::{self, File},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

// Redirect to files so a verbose child cannot block on a full output pipe.
fn run_cli(
    args: &[&str],
    cwd: &std::path::Path,
    rpc_port: Option<u16>,
) -> (std::process::ExitStatus, String) {
    let stdout = cwd.join("stdout.txt");
    let stderr = cwd.join("stderr.txt");
    let mut child = Command::new(env!("CARGO_BIN_EXE_surfpool"))
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::from(File::create(&stdout).unwrap()))
        .stderr(Stdio::from(File::create(&stderr).unwrap()))
        .spawn()
        .expect("start surfpool");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut rpc_observed = false;
    let status = loop {
        if let Some(port) = rpc_port {
            rpc_observed |= TcpStream::connect(("127.0.0.1", port)).is_ok();
        }
        if let Some(status) = child.try_wait().expect("poll surfpool") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("surfpool did not exit within 30 seconds");
        }
        thread::sleep(Duration::from_millis(50));
    };
    let output = format!(
        "{}\n{}",
        fs::read_to_string(stdout).unwrap(),
        fs::read_to_string(stderr).unwrap()
    );
    assert!(
        !rpc_observed,
        "RPC became available before failure: {output}"
    );
    (status, output)
}

#[test]
fn strict_snapshot_offline_missing_account_fails_startup() {
    let dir = tempfile::tempdir().unwrap();
    let address = "11111111111111111111111111111111";
    fs::write(
        dir.path().join("snapshot.json"),
        format!("{{\"{address}\":null}}"),
    )
    .unwrap();
    let ports = free_ports();
    let rpc_port = ports[0].to_string();
    let ws_port = ports[1].to_string();
    let studio_port = ports[2].to_string();
    let (status, output) = run_cli(
        &[
            "start",
            "--port",
            &rpc_port,
            "--ws-port",
            &ws_port,
            "--studio-port",
            &studio_port,
            "--offline",
            "--no-deploy",
            "--no-tui",
            "--no-studio",
            "--airdrop-amount",
            "0",
            "--snapshot",
            "snapshot.json",
            "--strict-snapshot",
        ],
        dir.path(),
        Some(ports[0]),
    );
    assert!(!status.success(), "unexpected success: {output}");
    assert!(
        output.contains(address),
        "missing account address: {output}"
    );
    assert!(
        !output.contains("Preloaded"),
        "unexpected preload success: {output}"
    );
    assert!(
        !output.contains("Loaded 1 accounts from snapshot file"),
        "unexpected load success: {output}"
    );
}

#[test]
fn malformed_snapshot_fails_startup() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("snapshot.json"), "{malformed").unwrap();
    let (status, output) = run_cli(
        &[
            "start",
            "--offline",
            "--no-deploy",
            "--no-tui",
            "--no-studio",
            "--airdrop-amount",
            "0",
            "--snapshot",
            "snapshot.json",
            "--strict-snapshot",
        ],
        dir.path(),
        None,
    );
    assert!(!status.success(), "unexpected success: {output}");
    assert!(output.contains("Failed to parse snapshot JSON"), "{output}");
}

#[test]
fn strict_snapshot_requires_snapshot_file() {
    let dir = tempfile::tempdir().unwrap();
    let (status, output) = run_cli(&["start", "--strict-snapshot"], dir.path(), None);
    assert!(!status.success(), "unexpected success: {output}");
    assert!(output.contains("--snapshot"), "{output}");
}

// Hold the sockets together until all ports have been selected.
fn free_ports() -> [u16; 3] {
    let listeners: Vec<_> = (0..3)
        .map(|_| TcpListener::bind(("127.0.0.1", 0)).unwrap())
        .collect();
    std::array::from_fn(|i| listeners[i].local_addr().unwrap().port())
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn rpc_is_ready(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"surfnet_getSurfnetInfo"}"#;
    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() {
        return false;
    }
    let Some((_, body)) = response.split_once("\r\n\r\n") else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/result/value/startup/phase")
                .and_then(|p| p.as_str())
                .map(str::to_owned)
        })
        .as_deref()
        == Some("ready")
}

fn assert_snapshot_reaches_ready(snapshot: &str, strict: bool) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("snapshot.json"), snapshot).unwrap();
    let ports = free_ports();
    let stdout = File::create(dir.path().join("stdout.txt")).unwrap();
    let stderr = File::create(dir.path().join("stderr.txt")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_surfpool"));
    command.args([
        "start",
        "--offline",
        "--no-deploy",
        "--no-tui",
        "--no-studio",
        "--airdrop-amount",
        "0",
        "--snapshot",
        "snapshot.json",
        "--port",
        &ports[0].to_string(),
        "--ws-port",
        &ports[1].to_string(),
        "--studio-port",
        &ports[2].to_string(),
    ]);
    if strict {
        command.arg("--strict-snapshot");
    }
    let mut child = ChildGuard(
        command
            .current_dir(dir.path())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if rpc_is_ready(ports[0]) {
            break;
        }
        let exited = child.0.try_wait().unwrap();
        if exited.is_some() || Instant::now() >= deadline {
            let output = format!(
                "{}\n{}",
                fs::read_to_string(dir.path().join("stdout.txt")).unwrap(),
                fs::read_to_string(dir.path().join("stderr.txt")).unwrap()
            );
            panic!(
                "snapshot startup did not reach ready (strict: {strict}, exit: {exited:?}): {output}"
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn default_snapshot_mode_reaches_ready_with_unavailable_account() {
    assert_snapshot_reaches_ready(r#"{"11111111111111111111111111111111":null}"#, false);
}

#[test]
fn strict_snapshot_mode_reaches_ready_with_valid_account() {
    assert_snapshot_reaches_ready(
        r#"{"4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi":{"lamports":1000000,"owner":"11111111111111111111111111111111","executable":false,"rentEpoch":0,"data":"AQID","parsedData":null}}"#,
        true,
    );
}
