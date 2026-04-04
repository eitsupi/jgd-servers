//! Client commands: `list`, `api plots`, `api render`, `api shutdown`.
//!
//! Communicates with a running jgdmr server via its REST API (over Unix
//! socket or TCP) or via OS signals (for shutdown).

use anyhow::{Context, Result, bail};
use jgd_protocol::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------------------------------------------------------------------------
// Target resolution
// ---------------------------------------------------------------------------

/// Resolve `--target` to a [`SocketAddr`] and optional PID.
///
/// Priority:
/// 1. Explicit URI (`unix://...`, `tcp://...`)
/// 2. Explicit PID → look up in discovery file
/// 3. None → auto-detect from discovery file (must be exactly one server)
pub fn resolve_target(target: Option<&str>) -> Result<(SocketAddr, Option<u32>)> {
    match target {
        Some(s) if s.contains("://") => {
            let addr = SocketAddr::parse(s).map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok((addr, None))
        }
        Some(s) => {
            // Treat as PID.
            let pid: u32 = s.parse().context("--target must be a URI or PID")?;
            let info = read_discovery()?;
            if info.pid != pid {
                bail!(
                    "discovery file PID ({}) does not match target PID ({pid})",
                    info.pid
                );
            }
            let addr = listen_addr_from_discovery(&info)?;
            Ok((addr, Some(pid)))
        }
        None => {
            let info = read_discovery()?;
            let pid = info.pid;
            let addr = listen_addr_from_discovery(&info)?;
            Ok((addr, Some(pid)))
        }
    }
}

fn read_discovery() -> Result<jgd_server::discovery::DiscoveryInfo> {
    let path = jgd_server::discovery::default_path()
        .context("could not determine discovery file path")?;
    jgd_server::discovery::read(&path)
        .with_context(|| format!("failed to read discovery file: {}", path.display()))
}

fn listen_addr_from_discovery(
    info: &jgd_server::discovery::DiscoveryInfo,
) -> Result<SocketAddr> {
    // Try listenUrl from serverInfo (headless mode).
    if let Some(si) = &info.server_info {
        if let Some(serde_json::Value::String(url)) = si.get("listenUrl") {
            return SocketAddr::parse(url).map_err(|e| anyhow::anyhow!("{e}"));
        }
        // Fall back to httpUrl (http mode).
        if let Some(serde_json::Value::String(url)) = si.get("httpUrl") {
            // httpUrl is like "http://127.0.0.1:3000/" — convert to tcp://.
            let addr = url
                .strip_prefix("http://")
                .and_then(|s| s.strip_suffix('/'))
                .unwrap_or(url);
            return Ok(SocketAddr::Tcp(addr.to_string()));
        }
    }
    bail!(
        "discovery file has no listenUrl or httpUrl in serverInfo \
         (is the server running in headless or http mode?)"
    );
}

// ---------------------------------------------------------------------------
// list subcommand
// ---------------------------------------------------------------------------

pub fn run_list(json: bool) -> Result<()> {
    let path = jgd_server::discovery::default_path()
        .context("could not determine discovery file path")?;

    let info = match jgd_server::discovery::read(&path) {
        Ok(info) => {
            // Verify the process is still alive.
            if !jgd_server::discovery::is_process_alive(info.pid) {
                if json {
                    println!("[]");
                } else {
                    println!("No active sessions (stale discovery file for PID {})", info.pid);
                }
                return Ok(());
            }
            info
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if json {
                println!("[]");
            } else {
                println!("No active sessions");
            }
            return Ok(());
        }
        Err(e) => return Err(e).context("failed to read discovery file"),
    };

    if json {
        let sessions = serde_json::json!([{
            "server_name": info.server_name,
            "pid": info.pid,
            "socket_path": info.socket_path,
            "server_info": info.server_info,
        }]);
        println!("{}", serde_json::to_string_pretty(&sessions)?);
    } else {
        let listen = info
            .server_info
            .as_ref()
            .and_then(|si| si.get("listenUrl").or_else(|| si.get("httpUrl")))
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        println!(
            "{name}  pid={pid}  socket={socket}  listen={listen}",
            name = info.server_name,
            pid = info.pid,
            socket = info.socket_path,
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// api plots
// ---------------------------------------------------------------------------

pub async fn run_api_plots(target: Option<&str>, json: bool) -> Result<()> {
    let (addr, _pid) = resolve_target(target)?;
    let body = http_get(&addr, "/plots").await?;

    if json {
        // Already JSON from the server — print as-is.
        let s = String::from_utf8(body).context("invalid UTF-8 in response")?;
        println!("{s}");
    } else {
        let plots: Vec<serde_json::Value> =
            serde_json::from_slice(&body).context("invalid JSON in response")?;
        if plots.is_empty() {
            println!("No plots");
        } else {
            for p in &plots {
                println!(
                    "{id}  {w}x{h}  ops={ops}",
                    id = p["session_id"].as_str().unwrap_or("?"),
                    w = p["width"],
                    h = p["height"],
                    ops = p["op_count"],
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// api render
// ---------------------------------------------------------------------------

pub async fn run_api_render(
    target: Option<&str>,
    plot_id: Option<&str>,
    format: &str,
    width: Option<f64>,
    height: Option<f64>,
    output: Option<&str>,
) -> Result<()> {
    let (addr, _pid) = resolve_target(target)?;

    // If no plot_id, fetch the plot list and use the first one.
    let id = match plot_id {
        Some(id) => id.to_string(),
        None => {
            let body = http_get(&addr, "/plots").await?;
            let plots: Vec<serde_json::Value> =
                serde_json::from_slice(&body).context("invalid JSON")?;
            plots
                .first()
                .and_then(|p| p["session_id"].as_str())
                .map(|s| s.to_string())
                .context("no plots available")?
        }
    };

    let mut path = format!("/plots/{id}/{format}");
    let mut params = Vec::new();
    if let Some(w) = width {
        params.push(format!("width={w}"));
    }
    if let Some(h) = height {
        params.push(format!("height={h}"));
    }
    if !params.is_empty() {
        path.push('?');
        path.push_str(&params.join("&"));
    }

    let body = http_get(&addr, &path).await?;

    match output {
        Some("-") | None => {
            use std::io::Write;
            std::io::stdout().write_all(&body)?;
        }
        Some(file_path) => {
            std::fs::write(file_path, &body)
                .with_context(|| format!("failed to write to {file_path}"))?;
            eprintln!("Wrote {} bytes to {file_path}", body.len());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// api shutdown
// ---------------------------------------------------------------------------

pub fn run_api_shutdown(target: Option<&str>) -> Result<()> {
    let (_addr, pid) = resolve_target(target)?;

    let pid = pid.context(
        "shutdown requires a PID (use --target PID or ensure a discovery file exists)",
    )?;

    if !jgd_server::discovery::is_process_alive(pid) {
        bail!("process {pid} is not alive");
    }

    send_signal(pid)?;
    eprintln!("Sent shutdown signal to PID {pid}");
    Ok(())
}

#[cfg(unix)]
fn send_signal(pid: u32) -> Result<()> {
    // SIGTERM for graceful shutdown.
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if ret != 0 {
        bail!(
            "kill({pid}, SIGTERM) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn send_signal(pid: u32) -> Result<()> {
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string()])
        .status()
        .context("failed to run taskkill")?;
    if !status.success() {
        bail!("taskkill failed with {status}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 client
// ---------------------------------------------------------------------------

async fn http_get(addr: &SocketAddr, path: &str) -> Result<Vec<u8>> {
    let request = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Connection: close\r\n\
         \r\n"
    );

    let response = match addr {
        #[cfg(unix)]
        SocketAddr::Unix(sock_path) => {
            let mut stream = tokio::net::UnixStream::connect(sock_path)
                .await
                .with_context(|| format!("failed to connect to {addr}"))?;
            stream.write_all(request.as_bytes()).await?;
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).await?;
            buf
        }
        SocketAddr::Tcp(host_port) => {
            let mut stream = tokio::net::TcpStream::connect(host_port.as_str())
                .await
                .with_context(|| format!("failed to connect to {addr}"))?;
            stream.write_all(request.as_bytes()).await?;
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).await?;
            buf
        }
        #[allow(unreachable_patterns)]
        _ => bail!("connecting to {addr} is not supported on this platform"),
    };

    parse_http_response(&response)
}

fn parse_http_response(raw: &[u8]) -> Result<Vec<u8>> {
    // Find end of headers.
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("invalid HTTP response: no header terminator")?;

    let header_bytes = &raw[..header_end];
    let header_str =
        std::str::from_utf8(header_bytes).context("invalid UTF-8 in HTTP headers")?;

    // Parse status line.
    let status_line = header_str
        .lines()
        .next()
        .context("empty HTTP response")?;
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .context("malformed status line")?
        .parse()
        .context("invalid status code")?;

    if status_code != 200 {
        let body = &raw[header_end + 4..];
        let body_str = String::from_utf8_lossy(body);
        bail!("HTTP {status_code}: {body_str}");
    }

    Ok(raw[header_end + 4..].to_vec())
}
