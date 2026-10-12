import assert from "node:assert/strict";
import { runInNewContext } from "node:vm";

interface Plot {
  session_id: string;
  plot_index: number;
  plot_id: number;
  revision: number;
  connected: boolean;
}

class Element {
  src: string | undefined;
  textContent = "";
  disabled = false;
  title = "";
  clientWidth = 800;
  clientHeight = 600;
  classList = { add() {}, remove() {} };
  listeners = new Map<string, () => void>();
  addEventListener(name: string, callback: () => void) {
    this.listeners.set(name, callback);
  }
  getAttribute(name: string) {
    return name === "src" ? this.src : undefined;
  }
  removeAttribute(name: string) {
    if (name === "src") this.src = undefined;
  }
}

async function viewer() {
  const source = await Deno.readTextFile(
    new URL("../crates/jgdmr/assets/app.js", import.meta.url),
  );
  const elements = new Map<string, Element>();
  const sockets: Socket[] = [];
  const timers = new Map<number, { callback: () => void; delay: number }>();
  const requests: {
    resolve: (response: { ok: boolean; json: () => Plot[] }) => void;
  }[] = [];
  let timerId = 0;
  let resize = () => {};
  class Socket {
    static OPEN = 1;
    readyState = 1;
    sent: string[] = [];
    onopen = () => {};
    onclose = () => {};
    onmessage = (_event: { data: string }) => {};
    constructor() {
      sockets.push(this);
    }
    send(message: string) {
      this.sent.push(message);
    }
  }
  runInNewContext(source, {
    document: {
      getElementById(id: string) {
        const element = new Element();
        elements.set(id, element);
        return element;
      },
    },
    location: { protocol: "http:", host: "localhost" },
    WebSocket: Socket,
    ResizeObserver: class {
      constructor(callback: () => void) {
        resize = callback;
      }
      observe() {}
    },
    fetch: () => new Promise((resolve) => requests.push({ resolve })),
    setTimeout(callback: () => void, delay: number) {
      timers.set(++timerId, { callback, delay });
      return timerId;
    },
    clearTimeout(id: number) {
      timers.delete(id);
    },
  });
  const tick = async (delay: number) => {
    for (const [id, timer] of [...timers]) {
      if (timer.delay === delay) {
        timers.delete(id);
        timer.callback();
      }
    }
    // Drain the snapshot promise chain, including finally.
    for (let i = 0; i < 10; i++) await Promise.resolve();
  };
  const reply = async (plots: Plot[], ok = true) => {
    const request = requests.shift();
    assert(request, "Expected a snapshot request");
    request.resolve({ ok, json: () => plots });
    await tick(-1);
  };
  const event = (type: string) => {
    sockets.at(-1)!.onmessage({ data: JSON.stringify({ type }) });
  };
  const element = (id: string) => elements.get(id)!;
  return {
    sockets,
    requests,
    tick,
    reply,
    event,
    element,
    resize: () => resize(),
  };
}

const plot = (id: number, revision = 1, connected = true): Plot => ({
  session_id: "r-1",
  plot_index: id - 1,
  plot_id: id,
  revision,
  connected,
});

Deno.test("browser snapshots deduplicate, preserve selection and refresh revisions", async () => {
  const app = await viewer();
  await app.tick(0);
  await app.reply([plot(1), plot(2), plot(2)]);
  assert.equal(app.element("plot-info").textContent, "2 / 2");
  app.element("btn-prev").listeners.get("click")!();
  assert(app.element("plot-img").src?.includes("plot_id=1"));

  app.event("frame");
  await app.tick(0);
  await app.reply([plot(1, 2), plot(2), plot(3)]);
  assert.equal(app.element("plot-info").textContent, "1 / 3");
  assert(app.element("plot-img").src?.includes("revision=2"));

  app.event("close");
  await app.tick(0);
  await app.reply([plot(1, 2, false), plot(2, 1, false)]);
  assert.equal(app.element("plot-info").textContent, "1 / 2 (disconnected)");
  app.resize();
  await app.tick(300);
  assert.equal(app.sockets[0].sent.length, 0);

  app.sockets[0].onclose();
  await app.tick(2000);
  app.sockets[1].onopen();
  await app.tick(0);
  await app.reply([plot(2), plot(3)]);
  assert.equal(app.element("plot-info").textContent, "2 / 2");
  app.resize();
  await app.tick(300);
  assert.equal(JSON.parse(app.sockets[1].sent[0]).plotIndex, 2);

  app.event("close");
  await app.tick(0);
  await app.reply([]);
  assert.equal(app.element("plot-info").textContent, "No plots");
  assert.equal(app.element("plot-img").src, undefined);
});

Deno.test("browser serializes snapshots and retries transient failures", async () => {
  const app = await viewer();
  await app.tick(0);
  app.event("frame");
  app.event("frame");
  assert.equal(app.requests.length, 1);
  await app.reply([plot(1)]);
  assert.equal(app.element("plot-info").textContent, "1 / 1");
  await app.tick(0);
  await app.reply([plot(1), plot(2)]);
  assert.equal(app.element("plot-info").textContent, "2 / 2");

  app.event("frame");
  await app.tick(0);
  await app.reply([], false);
  await app.tick(2000);
  await app.reply([plot(1), plot(2, 2)]);
  assert(app.element("plot-img").src?.includes("revision=2"));
});

Deno.test("browser reconnect refetches an image when server identities repeat", async () => {
  const app = await viewer();
  app.sockets[0].onopen();
  await app.tick(0);
  await app.reply([plot(1)]);
  const before = app.element("plot-img").src;
  assert(before?.includes("epoch=1"));
  app.sockets[0].onclose();
  await app.tick(2000);
  app.sockets[1].onopen();
  await app.tick(0);
  await app.reply([plot(1)]);
  const after = app.element("plot-img").src;
  assert(after?.includes("epoch=2"));
  assert.notEqual(after, before);
  assert.equal(app.element("plot-info").textContent, "1 / 1");
});
