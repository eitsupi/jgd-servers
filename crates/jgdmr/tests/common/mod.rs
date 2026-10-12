use std::fs::File;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use jgd_server::discovery::{self, DiscoveryInfo};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Keep output out of pipes so a verbose child cannot block, and always reap it,
/// including when an assertion panics.
pub struct Server {
    pub child: Child,
    pub tmp: tempfile::TempDir,
}

impl Server {
    pub fn start(args: &[&str]) -> Self {
        Self::start_in(tempfile::tempdir().unwrap(), args)
    }

    pub fn start_in(tmp: tempfile::TempDir, args: &[&str]) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_jgdmr"))
            .args(args)
            .env("HOME", tmp.path())
            .env("USERPROFILE", tmp.path())
            .env("XDG_CACHE_HOME", tmp.path().join("cache"))
            .env("LOCALAPPDATA", tmp.path().join("cache"))
            .env("XDG_RUNTIME_DIR", tmp.path())
            // Exercise the stdout contract with logging explicitly enabled.
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(File::create(tmp.path().join("stdout")).unwrap())
            .stderr(File::create(tmp.path().join("stderr")).unwrap())
            .spawn()
            .expect("failed to start jgdmr");
        Self { child, tmp }
    }

    pub fn discovery_path(&self) -> PathBuf {
        #[cfg(target_os = "macos")]
        let cache = self.tmp.path().join("Library").join("Caches");
        #[cfg(not(target_os = "macos"))]
        let cache = self.tmp.path().join("cache");
        cache.join("jgd").join("discovery.json")
    }

    pub fn stdout(&self) -> String {
        std::fs::read_to_string(self.tmp.path().join("stdout")).unwrap()
    }

    pub fn stderr(&self) -> String {
        std::fs::read_to_string(self.tmp.path().join("stderr")).unwrap()
    }

    pub fn discovery(&mut self) -> DiscoveryInfo {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(info) = discovery::read(&self.discovery_path()) {
                return info;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "server exited before discovery: {}",
                self.stderr()
            );
            assert!(
                Instant::now() < deadline,
                "discovery timeout: {}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn startup(&mut self) -> serde_json::Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let stdout = self.stdout();
            if stdout.contains('\n') {
                return serde_json::from_str(&stdout)
                    .unwrap_or_else(|e| panic!("stdout must be one JSON object: {e}: {stdout}"));
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "server exited before startup JSON: {}",
                self.stderr()
            );
            assert!(
                Instant::now() < deadline,
                "startup timeout: {}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "exit timeout: {}", self.stderr());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    pub fn terminate(&mut self) {
        // The child is still owned by this guard and has not been reaped.
        assert_eq!(
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) },
            0
        );
        assert!(
            self.wait().success(),
            "SIGTERM shutdown failed: {}",
            self.stderr()
        );
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
