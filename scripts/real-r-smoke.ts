/**
 * Linux real-R smoke test. Requires a built jgdmr and Rscript with jgd.
 * The relay captures R's bytes verbatim, including real metrics requests and
 * incremental frames. Captures are diagnostics, not portable pixel goldens.
 * Uses only Deno APIs and built-in modules; no downloaded JS dependencies.
 */
import assert from "node:assert/strict";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

interface Plot {
  session_id: string;
  plot_index: number;
  op_count: number;
  width: number;
  height: number;
}
interface Message {
  type: string;
  incremental?: boolean;
  plot?: { ops: { str?: string }[] };
}
interface Process {
  child: Deno.ChildProcess;
  status?: Deno.CommandStatus;
  logs: Promise<void>;
}

const delay = (ms: number) => new Promise((done) => setTimeout(done, ms));

async function within<T>(
  promise: Promise<T>,
  ms: number,
  label: string,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`Timed out: ${label}`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function eventually<T>(
  check: () => Promise<T | undefined | false>,
  processes: Process[],
  label: string,
): Promise<T> {
  const deadline = performance.now() + 45_000;
  while (performance.now() < deadline) {
    const result = await check();
    if (result) return result;
    for (const process of processes) {
      assert(
        !process.status,
        `${label}: child exited with ${process.status?.code}`,
      );
    }
    await delay(50);
  }
  throw new Error(`Timed out: ${label}`);
}

async function exists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return false;
    throw error;
  }
}

async function spawn(
  command: string,
  args: string[],
  env: Record<string, string>,
  logPath: string,
): Promise<Process> {
  const log = await Deno.open(logPath, {
    create: true,
    truncate: true,
    write: true,
  });
  try {
    const child = new Deno.Command(command, {
      args,
      env,
      stdin: "null",
      stdout: "piped",
      stderr: "piped",
    }).spawn();
    // Serialize both streams into one file without buffering all diagnostics.
    let writes = Promise.resolve();
    const sink = async (stream: ReadableStream<Uint8Array>) => {
      for await (const bytes of stream) {
        writes = writes.then(() => writeAll(log, bytes));
        await writes;
      }
    };
    const logs = Promise.all([sink(child.stdout), sink(child.stderr)])
      .then(() => {}).finally(() => log.close());
    const process: Process = { child, logs };
    child.status.then((status) => {
      process.status = status;
    });
    // Observe failures immediately; awaiting logs in cleanup still propagates them.
    logs.catch(() => {});
    return process;
  } catch (error) {
    log.close();
    throw error;
  }
}

async function writeAll(
  writer: { write(bytes: Uint8Array): Promise<number> },
  bytes: Uint8Array,
) {
  let offset = 0;
  while (offset < bytes.length) {
    const written = await writer.write(bytes.subarray(offset));
    assert(written > 0, "Writer made no progress");
    offset += written;
  }
}

function terminate(process: Process, signal: Deno.Signal) {
  try {
    process.child.kill(signal);
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) throw error;
  }
}

Deno.test("real R: captured deltas, two pages, CJK, SVG/PNG and disconnect", async () => {
  const binary = resolve(Deno.env.get("JGDMR_BIN") ?? "target/debug/jgdmr");
  const output = resolve(
    Deno.env.get("JGD_TEST_OUTPUT") ?? ".test-results/r-integration",
  );
  await Deno.mkdir(output, { recursive: true });
  const state = await Deno.makeTempDir({ prefix: "jgd-real-r-" });
  const children: Process[] = [];
  let listener: Deno.UnixListener | undefined;
  let downstream: Deno.UnixConn | undefined;
  let upstream: Deno.UnixConn | undefined;
  let relay: Promise<void> | undefined;
  let relayError: unknown;
  let stopping = false;
  const close = (resource: { close(): void } | undefined) => {
    try {
      resource?.close();
    } catch (error) {
      if (!(error instanceof Deno.errors.BadResource)) throw error;
    }
  };
  try {
    const socket = `${state}/r.sock`;
    const captureSocket = `${state}/capture.sock`;
    const env = { XDG_CACHE_HOME: `${state}/cache` };
    listener = Deno.listen({ transport: "unix", path: captureSocket });
    const server = await spawn(
      binary,
      [
        "headless",
        "--socket",
        `unix://${socket}`,
        "--listen",
        "tcp://127.0.0.1:0",
        "--json",
      ],
      env,
      `${output}/server.log`,
    );
    children.push(server);
    const address = await eventually(
      async () => {
        for (
          const line of (await Deno.readTextFile(`${output}/server.log`)).split(
            "\n",
          )
        ) {
          try {
            const value = JSON.parse(line);
            if (typeof value?.listen === "string") return new URL(value.listen);
          } catch { /* Partial JSON or a diagnostic line. */ }
        }
      },
      children,
      "server readiness",
    );
    assert.equal(address.protocol, "tcp:");
    assert(address.port);
    const base = `http://${address.host}`;
    const request = async (path: string) => {
      const response = await fetch(base + path, {
        signal: AbortSignal.timeout(5000),
      });
      const bytes = new Uint8Array(await response.arrayBuffer());
      assert(response.ok, `${path}: HTTP ${response.status}`);
      return bytes;
    };
    const decoder = new TextDecoder();
    const plots = async (): Promise<Plot[]> =>
      JSON.parse(decoder.decode(await request("/plots")));
    assert.deepEqual(await plots(), []);

    const acceptor = listener;
    relay = (async () => {
      downstream = await acceptor.accept();
      upstream = await Deno.connect({ transport: "unix", path: socket });
      const fromR = downstream;
      const toServer = upstream;
      using capture = await Deno.open(`${output}/r-messages.jsonl`, {
        create: true,
        truncate: true,
        write: true,
      });
      const pump = async (
        source: Deno.UnixConn,
        target: Deno.UnixConn,
        record: boolean,
      ) => {
        const buffer = new Uint8Array(65536);
        while (true) {
          const size = await source.read(buffer);
          if (size === null) {
            await target.closeWrite();
            return;
          }
          const bytes = buffer.subarray(0, size);
          if (record) await writeAll(capture, bytes);
          await writeAll(target, bytes);
        }
      };
      // Preserve half-close semantics so R's final close message reaches the Hub.
      const pumps = [pump(fromR, toServer, true), pump(toServer, fromR, false)];
      try {
        await Promise.all(pumps);
      } finally {
        close(fromR);
        close(toServer);
        await Promise.allSettled(pumps);
      }
    })().catch((error) => {
      if (!stopping) relayError = error;
    });

    const r = await spawn(
      "Rscript",
      [
        "--vanilla",
        fileURLToPath(new URL("./real-r-plots.R", import.meta.url)),
        `unix://${captureSocket}`,
        state,
      ],
      env,
      `${output}/r.log`,
    );
    children.push(r);
    for (
      const [checkpoint, pageCount] of [["first", 1], ["second", 2]] as const
    ) {
      await eventually(
        () => exists(`${state}/${checkpoint}.ready`),
        children,
        `R ${checkpoint}`,
      );
      const snapshot = await eventually(
        async () => {
          if (relayError) throw relayError;
          const current = await plots();
          if (
            current.length >= pageCount &&
            current.every((plot) => plot.op_count > 0)
          ) {
            // R finishing its write is not a Hub delivery barrier. Wait for the
            // final text delta before exporting or allowing R to start a new page.
            const latest = current.reduce((a, b) =>
              a.plot_index > b.plot_index ? a : b
            );
            const sid = encodeURIComponent(latest.session_id);
            const svg = decoder.decode(
              await request(
                `/plots/${sid}/svg?plot_index=${latest.plot_index}`,
              ),
            );
            if (
              svg.includes(checkpoint === "first" ? "日本語" : "Second page")
            ) return await plots();
          }
        },
        children,
        "plot delivery",
      );
      assert.equal(
        snapshot.length,
        pageCount,
        "Unexpected or duplicated pages",
      );
      await Deno.writeTextFile(
        `${output}/${checkpoint}-plots.json`,
        JSON.stringify(snapshot, null, 2) + "\n",
      );
      for (const plot of snapshot) {
        assert.equal(plot.width, 384);
        assert.equal(plot.height, 288);
        const sid = encodeURIComponent(plot.session_id);
        const path = `/plots/${sid}`;
        const query = `?plot_index=${plot.plot_index}`;
        const prefix = `${output}/${checkpoint}-${plot.plot_index}`;
        const svg = await request(`${path}/svg${query}`);
        assert(
          decoder.decode(svg).includes("<svg") &&
            decoder.decode(svg).includes("</svg>"),
        );
        await Deno.writeFile(`${prefix}.svg`, svg);
        const png = await request(`${path}/png${query}`);
        assert.deepEqual([...png.subarray(0, 8)], [
          137,
          80,
          78,
          71,
          13,
          10,
          26,
          10,
        ]);
        const view = new DataView(png.buffer, png.byteOffset, png.byteLength);
        assert.equal(view.getUint32(16), 384);
        assert.equal(view.getUint32(20), 288);
        await Deno.writeFile(`${prefix}.png`, png);
      }
      await Deno.writeTextFile(`${state}/${checkpoint}.continue`, "");
    }
    assert(
      (await within(r.child.status, 15_000, "R exit")).success,
      "R failed; see r.log",
    );
    await within(relay, 5000, "relay finish");
    if (relayError) throw relayError;
    const messages: Message[] =
      (await Deno.readTextFile(`${output}/r-messages.jsonl`))
        .trim().split("\n").map((line) => JSON.parse(line));
    const frames = messages.filter((message) => message.type === "frame");
    assert(
      frames.some((frame) => frame.incremental),
      "No real incremental frames",
    );
    assert(
      messages.some((message) => message.type === "metrics_request"),
      "No real metrics requests",
    );
    assert(
      messages.some((message) => message.type === "close"),
      "No real close message",
    );
    assert(
      frames.some((frame) => frame.plot?.ops.some((op) => op.str === "日本語")),
      "Missing CJK text",
    );
    // PR1 documents current behavior; offline history retention is a later change.
    await eventually(
      async () => (await plots()).length === 0,
      [server],
      "disconnect cleanup",
    );
    console.log(
      `Real R smoke passed; ${messages.length} captured messages in ${output}`,
    );
  } finally {
    stopping = true;
    close(listener);
    close(downstream);
    close(upstream);
    // Settle every cleanup operation, even if one log sink or child fails.
    // In particular, an R log error must not leave the server running.
    const cleanup = await Promise.allSettled([
      ...children.map(async (process) => {
        if (!process.status) terminate(process, "SIGTERM");
        try {
          await within(process.child.status, 5000, "child shutdown");
        } catch {
          terminate(process, "SIGKILL");
          await within(process.child.status, 5000, "forced child shutdown");
        }
        await within(process.logs, 5000, "diagnostic log drain");
      }),
      within(Promise.resolve(relay), 5000, "relay shutdown"),
    ]);
    await Deno.remove(state, { recursive: true });
    const failures = cleanup.filter((result) => result.status === "rejected");
    assert.equal(
      failures.length,
      0,
      "Cleanup failures: " + failures.map((result) => result.reason).join("; "),
    );
  }
});
