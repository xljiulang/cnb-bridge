import assert from "node:assert/strict";
import http from "node:http";
import test from "node:test";
import { createAgentServer } from "../templates/cnb-ai-bridge/agent/agent.mjs";

function listen(server) {
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve(server.address().port));
  });
}

function close(server) {
  return new Promise((resolve) => {
    server.close(resolve);
    server.closeAllConnections?.();
  });
}

async function startAgent(options = {}) {
  const server = createAgentServer({
    port: 0,
    token: "test-token",
    repoSlug: "tester/cnb-ai-bridge",
    aiBase: "http://127.0.0.1:1",
    ...options,
  });
  const port = await listen(server);
  return { server, port };
}

test("serves only the health endpoint without workspace credentials", async () => {
  const { server, port } = await startAgent();
  try {
    const response = await fetch(`http://127.0.0.1:${port}/healthz`);
    assert.equal(response.status, 200);
    const payload = await response.json();
    assert.equal(payload.status, "ok");
    assert.equal(payload.version, "1.0.0");
    assert.equal(typeof payload.uptime, "number");
  } finally {
    await close(server);
  }
});

test("proxies arbitrary credentials and bodies and overrides authorization", async () => {
  let received = null;
  const upstream = http.createServer((req, res) => {
    let body = "";
    req.setEncoding("utf8");
    req.on("data", (chunk) => (body += chunk));
    req.on("end", () => {
      received = { url: req.url, headers: req.headers, body };
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify({ ok: true }));
    });
  });
  const upstreamPort = await listen(upstream);
  const { server, port } = await startAgent({
    aiBase: `http://127.0.0.1:${upstreamPort}`,
  });
  try {
    const response = await fetch(`http://127.0.0.1:${port}/chat/completions?trace=1`, {
      method: "POST",
      headers: { authorization: "Bearer client-anything", "content-type": "application/json" },
      body: JSON.stringify({
        model: "deepseek-v4.1-flash",
        custom: { value: 42 },
        stream: false,
      }),
    });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { ok: true });
    assert.equal(received.url, "/tester/cnb-ai-bridge/-/ai/chat/completions?trace=1");
    assert.equal(received.headers.authorization, "Bearer test-token");
    assert.deepEqual(JSON.parse(received.body), {
      model: "deepseek-v4.1-flash",
      custom: { value: 42 },
      stream: false,
    });
  } finally {
    await close(server);
    await close(upstream);
  }
});

test("passes SSE through without buffering", async () => {
  const upstream = http.createServer((req, res) => {
    req.resume();
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write("data: {\"chunk\":1}\n\n");
    setTimeout(() => {
      res.write("data: {\"chunk\":2}\n\n");
      res.end();
    }, 30);
  });
  const upstreamPort = await listen(upstream);
  const { server, port } = await startAgent({
    aiBase: `http://127.0.0.1:${upstreamPort}`,
  });
  try {
    const response = await fetch(`http://127.0.0.1:${port}/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model: "model-a", stream: true }),
    });
    assert.equal(response.status, 200);
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    const first = await reader.read();
    const firstText = decoder.decode(first.value, { stream: true });
    assert.match(firstText, /chunk":1/);
    assert.ok(!firstText.includes('chunk":2'));
    let rest = "";
    for (;;) {
      const next = await reader.read();
      if (next.done) break;
      rest += decoder.decode(next.value, { stream: true });
    }
    rest += decoder.decode();
    assert.match(rest, /chunk":2/);
  } finally {
    await close(server);
    await close(upstream);
  }
});

test("survives an upstream response stream error and keeps serving health", async () => {
  const upstream = http.createServer((req, res) => {
    req.resume();
    res.writeHead(200, { "content-type": "text/event-stream" });
    res.write('data: {"chunk":1}\n\n');
    setTimeout(() => {
      res.destroy(new Error("upstream response failed"));
    }, 10);
  });
  const upstreamPort = await listen(upstream);
  const { server, port } = await startAgent({
    aiBase: `http://127.0.0.1:${upstreamPort}`,
  });
  try {
    const started = Date.now();
    const response = await fetch(`http://127.0.0.1:${port}/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ model: "model-a", stream: true }),
      signal: AbortSignal.timeout(2000),
    });
    assert.equal(response.status, 200);
    await assert.rejects(response.text());
    assert.ok(
      Date.now() - started < 1000,
      "downstream connection should close promptly after the upstream response errors",
    );

    const health = await fetch(`http://127.0.0.1:${port}/healthz`);
    assert.equal(health.status, 200);
  } finally {
    await close(server);
    await close(upstream);
  }
});

test("only exposes health and chat completion routes", async () => {
  const { server, port } = await startAgent();
  try {
    const healthMethod = await fetch(`http://127.0.0.1:${port}/healthz`, {
      method: "POST",
    });
    assert.equal(healthMethod.status, 405);

    const chatMethod = await fetch(`http://127.0.0.1:${port}/chat/completions`);
    assert.equal(chatMethod.status, 405);

    for (const path of ["/models", "/models/deepseek-v4.1-flash", "/v1/chat/completions", "/"]) {
      const response = await fetch(`http://127.0.0.1:${port}${path}`);
      assert.equal(response.status, 404, path);
    }

    const options = await fetch(`http://127.0.0.1:${port}/chat/completions`, {
      method: "OPTIONS",
    });
    assert.equal(options.status, 405);
  } finally {
    await close(server);
  }
});

test("returns 503 when the workspace token is missing", async () => {
  const { server, port } = await startAgent({ token: undefined });
  try {
    const response = await fetch(`http://127.0.0.1:${port}/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    });
    assert.equal(response.status, 503);
    assert.equal((await response.json()).error.code, "token_missing");
  } finally {
    await close(server);
  }
});
