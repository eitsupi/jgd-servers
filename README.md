# jgd-servers

Rust servers and viewers for the **jgd R graphics device**. The `jgdmr` executable
receives drawing operations from R over a JSON Lines connection, retains plots,
and exposes them through a browser viewer, REST API, or terminal viewer.

This repository contains the server side. The R-side device is a separate
component: R needs the jgd package to produce graphics, while rendering and font
metrics live here. The R package is installed separately from this workspace.

HTTP PNG exports and the TUI use the same raster implementation. SVG output is
serialized separately; its text can be rendered with different fonts by the
viewing application. A native GUI and a separate GPU renderer are not implemented.

## Install from source

Install a current stable Rust toolchain with Cargo and the platform's native
linker/build tools, then run from this checkout:

```sh
cargo install --locked --path crates/jgdmr
```

On Linux, install Fontconfig and at least one usable font family (for example,
`fontconfig` and `fonts-dejavu-core` on Debian/Ubuntu). Font discovery uses the
system font collection, so text metrics and rendered text can differ between
machines. macOS and Windows also need usable installed fonts. The TUI requires
an interactive terminal; image quality depends on its graphics protocol support,
with a half-block fallback when no graphics protocol is detected.

## Run

### Browser viewer

```sh
jgdmr http
```

Open `http://127.0.0.1:3000/`. The default R connection uses a Unix-domain socket
on Linux/macOS or a Windows named pipe. The server writes connection information
to its discovery file for a compatible R client to find. Generate plots using
the separately installed jgd R device; starting this server alone creates no
sample plots.

Use `--host` and `--port` to choose the HTTP bind address, `--socket` to choose
the R connection URI, or `http --headless` to expose only the REST API.

### Headless REST API

Use an explicit loopback TCP API listener for a portable command:

```sh
jgdmr headless --listen tcp://127.0.0.1:3001 --json
```

The startup JSON includes `pid`, `listen`, and `socket_path`. The REST listener
(`--listen`) and the R graphics connection (`--socket`) are separate endpoints.
REST over TCP does not require the optional R TCP transport feature.
When `--listen` is omitted, the API uses a Unix-domain socket on Unix and an
ephemeral loopback TCP port on Windows. Startup JSON reports the bound address,
including the assigned port when requesting port zero. REST over Windows named
pipes is not implemented; use a TCP REST listener on Windows. An explicit Unix
R or REST socket path must not already exist. After an unclean exit, verify ownership
and that no server is using the path before manually removing a stale socket.

### Terminal viewer

```sh
jgdmr tui
```

Press `?` for help, `q` or Escape to quit, Left/Right to browse plot history,
and Tab/Shift-Tab to switch sessions. The TUI does not expose a REST API.

### Inspect and export

With an HTTP or headless server running and an R client having created a plot:

```sh
jgdmr list --json
jgdmr api plots --json
jgdmr api render --format svg --output plot.svg
jgdmr api render --format png --width 800 --height 600 --output plot.png
```

`api render` uses the first session returned by the plot list when no ID is
given, and renders that session's latest plot. Supply a session ID to select one
explicitly. `api --target tcp://127.0.0.1:3001 plots --json` selects a server
without discovery. The REST routes are `GET /plots`, `GET /plots/{id}/svg`, and
`GET /plots/{id}/png`; render routes accept `width`, `height`, and `plot_index`
query parameters. The browser mode additionally exposes a WebSocket endpoint.

### Optional R transport over TCP

R-side TCP transport is disabled by default. Build or run with the server crate's
feature enabled when using a `tcp://` R connection:

```sh
cargo run --locked -p jgdmr --features jgd-server/tcp -- http --socket tcp://127.0.0.1:7000
```

Keep listeners on loopback or local sockets. The server does not implement
network authentication or TLS; exposing it on an untrusted network can disclose
plots and allow untrusted clients to connect.

## Discovery and platform notes

Discovery uses one `discovery.json` file per user:

- Linux: `$XDG_CACHE_HOME/jgd/discovery.json`, falling back to
  `~/.cache/jgd/discovery.json`.
- macOS: `~/Library/Caches/jgd/discovery.json`.
- Windows: `%LOCALAPPDATA%/jgd/discovery.json` (with a user-profile fallback).

The discovery file describes the last server to write it, not a registry of all
running servers. `list` and automatic API targeting use that file. Use explicit
API targets when running multiple servers. Default Unix socket paths are created
in a protected runtime directory with a temporary-directory fallback; Windows R
connections use PID-based named pipes. URI forms are `unix:///path/to/socket`,
`npipe:////./pipe/name`, and `tcp://host:port`, subject to the transport and
platform restrictions above.

## Development

See [DEVELOPMENT.md](DEVELOPMENT.md) for the workspace architecture, build setup,
tests, CI coverage, and remaining validation gaps.
