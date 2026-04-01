//! Server discovery file for R client auto-connection.
//!
//! Writes a `discovery.json` to a platform-specific cache directory so that
//! R clients can find the running server without explicit configuration.
//!
//! - Linux: `$XDG_CACHE_HOME/jgd/discovery.json` or `~/.cache/jgd/discovery.json`
//! - macOS: `~/Library/Caches/jgd/discovery.json`
//! - Windows: `%LOCALAPPDATA%/jgd/discovery.json`

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Discovery file schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryInfo {
    pub server_name: String,
    pub socket_path: String,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Resolve the platform-specific cache directory for jgd.
///
/// Returns `None` if the home directory cannot be determined.
pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|h| h.join("Library").join("Caches").join("jgd"))
    }

    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join("AppData").join("Local")))
            .map(|d| d.join("jgd"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join(".cache")))
            .map(|d| d.join("jgd"))
    }
}

/// Resolve the default discovery file path.
pub fn default_path() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("discovery.json"))
}

/// Write the discovery file atomically (temp file + rename).
///
/// Creates parent directories if they don't exist.
pub fn write(path: &Path, info: &DiscoveryInfo) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(info)
        .map_err(io::Error::other)?;

    // Write to temp file in the same directory, then rename for atomicity.
    let tmp_name = format!(
        ".jgd-discovery-{}.tmp",
        std::process::id()
    );
    let tmp_path = path.with_file_name(tmp_name);

    std::fs::write(&tmp_path, json.as_bytes())?;

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        // Best-effort cleanup of temp file.
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    tracing::info!(?path, "discovery file written");
    Ok(())
}

/// Read and parse a discovery file.
pub fn read(path: &Path) -> io::Result<DiscoveryInfo> {
    let data = std::fs::read_to_string(path)?;
    serde_json::from_str(&data)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Remove the discovery file, but only if its PID matches the current process.
///
/// This prevents accidentally removing a file written by a different server
/// instance that started after us.
pub fn remove(path: &Path) -> io::Result<()> {
    match read(path) {
        Ok(info) if info.pid == std::process::id() => {
            std::fs::remove_file(path)?;
            tracing::info!(?path, "discovery file removed");
            Ok(())
        }
        Ok(_) => {
            tracing::debug!(?path, "discovery file owned by another process, skipping removal");
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            tracing::warn!(?path, %e, "failed to read discovery file for removal");
            // Best-effort: don't fail shutdown over this.
            Ok(())
        }
    }
}

/// Check if a process with the given PID is alive.
#[cfg(unix)]
pub fn is_process_alive(pid: u32) -> bool {
    // On Linux, /proc/<pid> exists iff the process is alive.
    // On macOS/other Unix, fall back to `kill -0`.
    Path::new(&format!("/proc/{pid}")).exists()
        || std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

#[cfg(windows)]
pub fn is_process_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Minimal cross-platform home directory lookup.
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("discovery.json");

        let info = DiscoveryInfo {
            server_name: "test".into(),
            socket_path: "/tmp/jgd-test.sock".into(),
            pid: std::process::id(),
            server_info: None,
        };

        write(&path, &info).unwrap();
        let read_back = read(&path).unwrap();
        assert_eq!(info, read_back);
    }

    #[test]
    fn write_is_atomic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub").join("discovery.json");

        let info = DiscoveryInfo {
            server_name: "test".into(),
            socket_path: "/tmp/jgd-test.sock".into(),
            pid: 12345,
            server_info: None,
        };

        // Should create parent directories.
        write(&path, &info).unwrap();
        assert!(path.exists());

        // No temp file should remain.
        let parent = path.parent().unwrap();
        let temps: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(temps.is_empty(), "temp file not cleaned up");
    }

    #[test]
    fn remove_only_own_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("discovery.json");

        // Write with a different PID.
        let info = DiscoveryInfo {
            server_name: "other".into(),
            socket_path: "/tmp/other.sock".into(),
            pid: 99999,
            server_info: None,
        };
        write(&path, &info).unwrap();

        // Remove should not delete (different PID).
        remove(&path).unwrap();
        assert!(path.exists(), "should not remove file owned by other PID");

        // Now write with our PID.
        let our_info = DiscoveryInfo {
            pid: std::process::id(),
            ..info
        };
        write(&path, &our_info).unwrap();
        remove(&path).unwrap();
        assert!(!path.exists(), "should remove file owned by our PID");
    }

    #[test]
    fn remove_nonexistent_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.json");
        remove(&path).unwrap();
    }

    #[test]
    fn cache_dir_is_some() {
        // Should resolve to something on any platform with HOME set.
        assert!(cache_dir().is_some());
    }

    #[test]
    fn server_info_serialized() {
        let mut server_info = serde_json::Map::new();
        server_info.insert(
            "httpUrl".into(),
            serde_json::Value::String("http://127.0.0.1:8080".into()),
        );

        let info = DiscoveryInfo {
            server_name: "jgdmr".into(),
            socket_path: "/tmp/jgd.sock".into(),
            pid: 1234,
            server_info: Some(server_info),
        };

        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("httpUrl"));
        assert!(json.contains("8080"));
    }

    #[cfg(unix)]
    #[test]
    fn current_process_is_alive() {
        assert!(is_process_alive(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn dead_pid_is_not_alive() {
        // PID 99999999 is extremely unlikely to exist.
        assert!(!is_process_alive(99999999));
    }
}
