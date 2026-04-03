//! Integration test: verify that `jgdmr http` writes the discovery file.

use std::process::{Command, Stdio};
use std::time::Duration;

use jgd_server::discovery::{self, DiscoveryInfo};

/// Start jgdmr http in headless mode and verify the discovery file.
#[test]
fn http_writes_discovery_file() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("test.sock");

    // Port 0 lets the OS assign a free port.
    let mut child = Command::new(env!("CARGO_BIN_EXE_jgdmr"))
        .args([
            "http",
            "--socket",
            socket.to_str().unwrap(),
            "--port",
            "0",
            "--headless",
        ])
        .env("HOME", tmp.path())
        .env("XDG_CACHE_HOME", tmp.path().join("cache"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start jgdmr");

    let pid = child.id();

    // Wait for the discovery file to appear.
    let discovery_path = tmp.path().join("cache").join("jgd").join("discovery.json");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if discovery_path.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Kill the child before assertions so it doesn't linger on failure.
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        discovery_path.exists(),
        "discovery file was not created at {}",
        discovery_path.display()
    );

    let info: DiscoveryInfo = discovery::read(&discovery_path).unwrap();
    assert_eq!(info.server_name, "jgdmr");
    assert_eq!(info.pid, pid);
    assert!(
        info.socket_path.starts_with("unix://"),
        "socketPath should be a unix:// URI, got: {}",
        info.socket_path
    );
    assert!(info.socket_path.contains("test.sock"));

    // Verify serverInfo contains httpUrl.
    let server_info = info.server_info.as_ref().expect("serverInfo missing");
    let http_url = server_info
        .get("httpUrl")
        .expect("httpUrl missing")
        .as_str()
        .unwrap();
    assert!(
        http_url.starts_with("http://"),
        "httpUrl should start with http://, got: {http_url}"
    );
}
