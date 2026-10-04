# Development

For installation and everyday usage, see the [README](README.md).
Run the commands below from the repository root.

## Architecture

- `jgd-protocol`: messages, drawing operations, graphics contexts, address parsing,
  and the JSONL codec.
- `jgd-font-metrics`: server-side text measurement and shaping using Parley and
  Fontique, with Skrifa glyph outlines.
- `jgd-render`: a shared **tiny-skia CPU raster backend** for PNG output and the
  TUI, plus a separate SVG serializer using quick-xml.
- `jgd-server`: R connection transports, sessions, plot history, the shared hub,
  and discovery.
- `jgdmr`: HTTP/browser, headless REST, TUI, and API client commands.

HTTP PNG exports and the TUI use the same raster implementation. SVG output is
serialized separately; its text can be rendered with different fonts by the
viewing application. A native GUI and a separate GPU renderer are not implemented.

## Build setup

Install a current stable Rust toolchain with Cargo and the platform's native
linker/build tools. This workspace uses Rust edition 2024; an older distribution
Rust package may not be sufficient. No minimum supported Rust version is currently
declared. Dependencies are pinned in `Cargo.lock`.

```sh
cargo build --locked --release -p jgdmr
cargo run --locked -p jgdmr -- --help
```

Or install the binary from this checkout:

```sh
cargo install --locked --path crates/jgdmr
```

The [usage examples](README.md#run) use the installed `jgdmr`. Without installing,
substitute `cargo run --locked -p jgdmr --` for `jgdmr`.

On Linux, install Fontconfig and at least one usable font family (for example,
`fontconfig` and `fonts-dejavu-core` on Debian/Ubuntu). Font discovery uses the
system font collection, so text metrics and rendered text can differ between
machines. macOS and Windows also need usable installed fonts. The TUI requires
an interactive terminal; image quality depends on its graphics protocol support,
with a half-block fallback when no graphics protocol is detected.

The normal Rust workspace tests do not require R.

## Tests and CI

```sh
cargo fmt --all -- --check
cargo test --locked --workspace
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features
```

[Check](.github/workflows/check.yml) runs the locked workspace tests with default and
all features on Linux, macOS, and Windows, plus formatting and Clippy on Linux.
Clippy warnings are reported but are not promoted to errors in this baseline.
The jobs select stable Rust via `rust-toolchain.toml` and cache Cargo dependencies,
following [arf](https://github.com/eitsupi/arf/blob/main/.github/workflows/check.yml).
The workflow is a cross-platform check, not a claim that every graphics or
transport path has end-to-end coverage.

Tests exercise protocol serialization/JSONL framing, server state and discovery,
font metrics, rendering, and CLI/HTTP behavior. Protocol clients constructed in
Rust tests are synthetic; they are not captured traffic from a real R process.
Platform-specific tests may be conditionally compiled.

### Real R integration

[Real R integration](.github/workflows/r-integration.yml) separately installs a
CRAN release of jgd on Ubuntu using `r-lib/actions/setup-r-dependencies` and runs
the Deno/TypeScript test [scripts/real-r-smoke.ts](scripts/real-r-smoke.ts). It relays and
captures genuine R JSONL, checks incremental drawing, two-page history, Japanese
text transport, device dimensions at 96 DPI, SVG/PNG exports, and disconnect
cleanup. Captures, renders, and diagnostics are uploaded as CI artifacts. This
is a smoke test, not a visual golden test or proof of Japanese font correctness.
To run it locally on Linux, install Deno 2, R and jgd from CRAN
(`install.packages("jgd")`), then build the debug binary expected by the harness:

```sh
cargo build --locked -p jgdmr
deno task check
deno task test:r
```

The harness uses only built-in modules, so it needs no JavaScript packages or
dependency lockfile. `deno task check` runs formatting, lint and type checks.
Set `JGDMR_BIN` or `JGD_TEST_OUTPUT` to override the binary or artifact directory.
Only base R and jgd are required.

### Remaining validation gaps

- Broader real-R interoperability across platforms, package versions and graphics operations.
- A representative, versioned graphics corpus with visual regression baselines.
- Cross-platform font fallback and text-layout equivalence, including complex
  scripts and missing fonts.
- Interactive browser/TUI visual checks and terminal graphics compatibility.
- Comprehensive Windows named-pipe connection and lifecycle coverage.

Do not treat successful unit tests as proof of pixel-identical output across
operating systems or complete R graphics compatibility.
