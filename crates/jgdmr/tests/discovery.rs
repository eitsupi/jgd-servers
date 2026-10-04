//! Process-level startup, readiness, transport and shutdown regressions.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use common::Server;

fn assert_default_r_transport(info: &jgd_server::discovery::DiscoveryInfo) {
    #[cfg(unix)]
    assert!(
        info.socket_path.starts_with("unix://"),
        "{}",
        info.socket_path
    );
    #[cfg(windows)]
    assert_eq!(
        info.socket_path,
        format!("npipe:////./pipe/jgdmr-{}", info.pid)
    );
}

fn assert_api_ready(address: &str) {
    let address = address
        .strip_prefix("tcp://")
        .or_else(|| address.strip_prefix("http://"))
        .expect("TCP or HTTP URI")
        .trim_end_matches('/');
    let socket_addr: std::net::SocketAddr = address.parse().unwrap();
    assert_ne!(
        socket_addr.port(),
        0,
        "readiness must advertise the bound port"
    );
    let mut stream = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET /plots HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}

#[test]
fn http_writes_discovery_file_with_bound_ephemeral_port() {
    let mut server = Server::start(&["http", "--port", "0", "--headless"]);
    let info = server.discovery();
    assert_eq!(info.server_name, "jgdmr");
    assert_eq!(info.pid, server.child.id());
    assert_default_r_transport(&info);
    let url = info.server_info.unwrap()["httpUrl"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_api_ready(&url);
}

#[test]
fn headless_json_is_clean_and_advertises_ready_ephemeral_tcp_api() {
    // Omitting --socket exercises Windows' default R named pipe as well as Unix.
    let mut server = Server::start(&["headless", "--listen", "tcp://127.0.0.1:0", "--json"]);
    let startup = server.startup();
    let info = server.discovery();
    assert_eq!(startup["pid"], server.child.id());
    assert_eq!(startup["socket_path"], info.socket_path);
    assert_default_r_transport(&info);
    assert_eq!(startup["listen"], info.server_info.unwrap()["listenUrl"]);
    assert_api_ready(startup["listen"].as_str().unwrap());
    // Parse the complete stream again after a request, not just its first line.
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&server.stdout()).unwrap(),
        startup
    );
    assert!(
        !server.stderr().is_empty(),
        "enabled logs should go to stderr"
    );
}

#[test]
fn headless_defaults_start_with_platform_supported_transports() {
    let mut server = Server::start(&["headless", "--json"]);
    let startup = server.startup();
    let info = server.discovery();
    assert_default_r_transport(&info);
    assert_eq!(startup["listen"], info.server_info.unwrap()["listenUrl"]);
    let api = startup["listen"].as_str().unwrap();
    #[cfg(windows)]
    {
        assert!(api.starts_with("tcp://127.0.0.1:"), "{api}");
        assert_api_ready(api);
        // Opening the advertised R pipe checks an actual listener, not only its URI.
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!(r"\\.\pipe\jgdmr-{}", server.child.id()))
            .expect("default R named pipe should accept a connection");
    }
    #[cfg(unix)]
    {
        let path = api.strip_prefix("unix://").expect("Unix API default");
        assert!(path.ends_with("-api.sock"));
        std::os::unix::net::UnixStream::connect(path)
            .expect("default Unix API should accept a connection");
    }
}

#[test]
fn failed_api_bind_does_not_publish_readiness() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let uri = format!("tcp://{}", occupied.local_addr().unwrap());
    let mut server = Server::start(&["headless", "--listen", &uri, "--json"]);
    assert!(!server.wait().success());
    assert!(
        server.stdout().is_empty(),
        "false readiness: {}",
        server.stdout()
    );
    assert!(!server.discovery_path().exists());
    #[cfg(unix)]
    assert!(
        !server
            .tmp
            .path()
            .join("jgdmr")
            .join(format!("{}.sock", server.child.id()))
            .exists(),
        "R socket leaked on API bind failure"
    );
}

#[test]
fn failed_http_bind_cleans_up_r_listener_without_discovery() {
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port().to_string();
    let mut server = Server::start(&["http", "--host", "127.0.0.1", "--port", &port, "--headless"]);
    assert!(!server.wait().success());
    assert!(!server.discovery_path().exists());
    #[cfg(unix)]
    assert!(
        !server
            .tmp
            .path()
            .join("jgdmr")
            .join(format!("{}.sock", server.child.id()))
            .exists(),
        "R socket leaked on HTTP bind failure"
    );
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::os::unix::net::{UnixListener, UnixStream};

    #[test]
    fn socket_bind_preserves_regular_file() {
        for flag in ["--listen", "--socket"] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("api.sock");
            std::fs::write(&path, b"keep me").unwrap();
            let uri = format!("unix://{}", path.display());
            let mut server = Server::start_in(tmp, &["headless", flag, &uri, "--json"]);
            assert!(!server.wait().success());
            assert_eq!(std::fs::read(path).unwrap(), b"keep me");
            assert!(server.stdout().is_empty());
            assert!(!server.discovery_path().exists());
        }
    }

    #[test]
    fn socket_bind_preserves_symlink_and_target() {
        for flag in ["--listen", "--socket"] {
            let tmp = tempfile::tempdir().unwrap();
            let target = tmp.path().join("target");
            let path = tmp.path().join("api.sock");
            std::fs::write(&target, b"keep target").unwrap();
            symlink(&target, &path).unwrap();
            let uri = format!("unix://{}", path.display());
            let mut server = Server::start_in(tmp, &["headless", flag, &uri, "--json"]);
            assert!(!server.wait().success());
            assert_eq!(std::fs::read_link(path).unwrap(), target);
            assert_eq!(std::fs::read(target).unwrap(), b"keep target");
            assert!(server.stdout().is_empty());
        }
    }

    #[test]
    fn socket_bind_preserves_live_socket() {
        for flag in ["--listen", "--socket"] {
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("api.sock");
            let _listener = UnixListener::bind(&path).unwrap();
            let inode = std::fs::symlink_metadata(&path).unwrap().ino();
            let uri = format!("unix://{}", path.display());
            let mut server = Server::start_in(tmp, &["headless", flag, &uri, "--json"]);
            assert!(!server.wait().success());
            assert_eq!(std::fs::symlink_metadata(&path).unwrap().ino(), inode);
            UnixStream::connect(path).expect("original listener should remain reachable");
            assert!(server.stdout().is_empty());
        }
    }

    #[test]
    fn sigterm_cleans_headless_discovery_and_both_sockets() {
        let mut server = Server::start(&["headless", "--json"]);
        let startup = server.startup();
        let r_path = startup["socket_path"]
            .as_str()
            .unwrap()
            .strip_prefix("unix://")
            .unwrap();
        let api_path = startup["listen"]
            .as_str()
            .unwrap()
            .strip_prefix("unix://")
            .unwrap();
        assert!(std::path::Path::new(r_path).exists());
        assert!(std::path::Path::new(api_path).exists());
        assert!(server.discovery_path().exists());
        server.terminate();
        assert!(!std::path::Path::new(r_path).exists());
        assert!(!std::path::Path::new(api_path).exists());
        assert!(!server.discovery_path().exists());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&server.stdout()).unwrap(),
            startup
        );
    }

    #[test]
    fn sigterm_cleans_http_discovery_and_r_socket() {
        let mut server = Server::start(&["http", "--port", "0", "--headless"]);
        let info = server.discovery();
        let r_path = info.socket_path.strip_prefix("unix://").unwrap();
        assert!(std::path::Path::new(r_path).exists());
        assert_api_ready(
            info.server_info.as_ref().unwrap()["httpUrl"]
                .as_str()
                .unwrap(),
        );
        server.terminate();
        assert!(!std::path::Path::new(r_path).exists());
        assert!(!server.discovery_path().exists());
    }
}
