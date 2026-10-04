#!/usr/bin/env python3
"""Linux real-R integration smoke test; captures authentic JGD JSONL.

Requires a built jgdmr and Rscript with jgd installed. No Python packages.
The relay records R's actual output, including metrics requests and deltas;
it does not generate or normalize protocol messages. Outputs are CI artifacts,
not portable golden pixel references (system fonts can differ).
"""

import argparse
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import tempfile
import threading
import time
import urllib.parse
import urllib.request


def eventually(check, processes, label, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        for proc in processes:
            if proc.poll() is not None:
                raise RuntimeError(f"{label}: process exited with {proc.returncode}")
        time.sleep(0.05)
    raise TimeoutError(label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="target/debug/jgdmr")
    parser.add_argument("--output", default=".test-results/r-integration")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    children = []
    relay_errors = []
    stop = threading.Event()
    relay_thread = None

    with tempfile.TemporaryDirectory(prefix="jgd-real-r-") as tmp:
        state = Path(tmp)
        r_socket = state / "r.sock"
        proxy_socket = state / "capture.sock"
        env = dict(os.environ, XDG_CACHE_HOME=str(state / "cache"))
        with (output / "server.log").open("w+") as server_log, \
             (output / "r.log").open("w") as r_log, \
             socket.socket(socket.AF_UNIX) as proxy:
            proxy.bind(str(proxy_socket))
            proxy.listen(1)
            proxy.settimeout(0.2)
            try:
                server = subprocess.Popen(
                    [binary, "headless", "--socket", f"unix://{r_socket}",
                     "--listen", "tcp://127.0.0.1:0", "--json"],
                    stdout=server_log, stderr=subprocess.STDOUT, env=env,
                )
                children.append(server)

                def startup_info():
                    for line in (output / "server.log").read_text().splitlines():
                        try:
                            value = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        if isinstance(value, dict) and "listen" in value:
                            return value
                    return None

                info = eventually(startup_info, children, "server readiness")
                address = urllib.parse.urlparse(info["listen"])
                assert address.scheme == "tcp" and address.port, info
                base = "http://" + address.netloc

                def request(path):
                    with urllib.request.urlopen(base + path, timeout=5) as response:
                        return response.read()

                assert json.loads(request("/plots")) == []

                def relay():
                    try:
                        while not stop.is_set():
                            try:
                                conn, _ = proxy.accept()
                                break
                            except TimeoutError:
                                continue
                        else:
                            return
                        with conn, socket.socket(socket.AF_UNIX) as upstream, \
                             (output / "r-messages.jsonl").open("wb") as capture:
                            upstream.connect(str(r_socket))
                            while not stop.is_set():
                                readable, _, _ = select.select([conn, upstream], [], [], 0.2)
                                for source in readable:
                                    data = source.recv(65536)
                                    if not data:
                                        return
                                    if source is conn:
                                        capture.write(data)
                                        capture.flush()
                                        upstream.sendall(data)
                                    else:
                                        conn.sendall(data)
                    except Exception as exc:
                        relay_errors.append(repr(exc))

                relay_thread = threading.Thread(target=relay, daemon=True)
                relay_thread.start()
                r = subprocess.Popen(
                    ["Rscript", "--vanilla", str(Path(__file__).with_name("real-r-plots.R")),
                     f"unix://{proxy_socket}", str(state)],
                    stdout=r_log, stderr=subprocess.STDOUT, env=env,
                )
                children.append(r)
                for checkpoint, minimum_pages in [("first", 1), ("second", 2)]:
                    eventually(lambda: (state / f"{checkpoint}.ready").exists(),
                               children, f"R {checkpoint} checkpoint")

                    def ready_plots():
                        plots = json.loads(request("/plots"))
                        if len(plots) >= minimum_pages and all(p["op_count"] > 0 for p in plots):
                            # R finishing a write does not mean the relay/Hub has
                            # consumed its last delta. Wait for the final text
                            # operation before exporting the checkpoint.
                            latest = max(plots, key=lambda p: p["plot_index"])
                            sid = urllib.parse.quote(latest["session_id"], safe="")
                            svg = request(f"/plots/{sid}/svg?plot_index={latest['plot_index']}")
                            marker = "日本語" if checkpoint == "first" else "Second page"
                            if marker.encode("utf-8") in svg:
                                return plots
                        return None

                    plots = eventually(ready_plots, children, "plot delivery")
                    (output / f"{checkpoint}-plots.json").write_text(json.dumps(plots, indent=2))
                    for plot in plots:
                        assert plot["width"] == 384 and plot["height"] == 288, plot
                        sid = urllib.parse.quote(plot["session_id"], safe="")
                        index = plot["plot_index"]
                        prefix = f"{checkpoint}-{index}"
                        svg = request(f"/plots/{sid}/svg?plot_index={index}")
                        assert b"<svg" in svg and b"</svg>" in svg
                        (output / f"{prefix}.svg").write_bytes(svg)
                        png = request(f"/plots/{sid}/png?plot_index={index}")
                        assert png[:8] == b"\x89PNG\r\n\x1a\n"
                        assert struct.unpack(">II", png[16:24]) == (384, 288)
                        (output / f"{prefix}.png").write_bytes(png)
                    (state / f"{checkpoint}.continue").touch()

                assert r.wait(timeout=15) == 0, "R script failed; see r.log"
                relay_thread.join(timeout=5)
                assert not relay_thread.is_alive(), "relay failed to finish"
                assert not relay_errors, relay_errors
                messages = [json.loads(line) for line in
                            (output / "r-messages.jsonl").read_text().splitlines()]
                frames = [m for m in messages if m["type"] == "frame"]
                assert frames and any(m.get("incremental") for m in frames)
                assert any(m["type"] == "metrics_request" for m in messages)
                assert any(m["type"] == "close" for m in messages)
                assert any(op.get("str") == "日本語" for m in frames for op in m["plot"]["ops"])
                # PR1 documents current behavior; retained offline history is a later change.
                eventually(lambda: json.loads(request("/plots")) == [], [server], "disconnect cleanup")
                print(f"Real R smoke passed; {len(messages)} captured messages in {output}")
            finally:
                stop.set()
                for proc in reversed(children):
                    if proc.poll() is None:
                        proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()
                if relay_thread is not None:
                    relay_thread.join(timeout=2)


if __name__ == "__main__":
    main()
