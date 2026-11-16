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
  return new Promise((resolve) => server.close(resolve));
}

async function startAgent(options = {}) {
  const server = createAgentServer({
    port: 0,
    token: "test-token",
    repoSlug: "tester/cnb-ai-bridge",
    aiBase: "http://127.0.0.1:1",
    models: [
      { id: "model-a", object: "model", created: 0, owned_by: "cnb" },
      { id: "model-b", object: "model", created: 0, owned_by: "cnb" },
    ],
    ...options,
  });
  const port = await listen(server);
  return { server, port };
}

test("serves models under both prefixes", async () => {
  const { server, port } = await startAgent();
  try {
    for (const prefix of ["", "/v1"]) {
      const list = await fetch(`http://127.0.0.1:${port}${prefix}/models`);
      assert.equal(list.status, 200);
      const payload = await list.json();
      assert.equal(payload.object, "list");
      assert.equal(payload.data.length, 2);

      const one = await fetch(`http://127.0.0.1:${port}${prefix}/models/model-a`);
      assert.equal(one.status, 200);
      assert.equal((await one.json()).id, "model-a");

      const missing = await fetch(`http://127.0.0.1:${port}${prefix}/models/missing`);
      assert.equal(missing.status, 404);
      assert.equal((await missing.json()).error.code, "model_not_found");
    }
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
    const response = await fetch(`http://127.0.0.1:${port}/v1/chat/completions?trace=1`, {
      method: "POST",
      headers: { authorization: "Bearer client-anything", "content-type": "application/json" },
      body: JSON.stringify({ model: "any-model", custom: { value: 42 }, stream: false }),
    });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { ok: true });
    assert.equal(received.url, "/tester/cnb-ai-bridge/-/ai/chat/completions?trace=1");
    assert.equal(received.headers.authorization, "Bearer test-token");
    assert.deepEqual(JSON.parse(received.body), {
      model: "any-model",
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
    const text = await response.text();
    assert.match(text, /chunk/);
    assert.match(text, /chunk[^]*chunk/);
  } finally {
    await close(server);
    await close(upstream);
  }
});

test("returns 405 and 404 for invalid methods and paths", async () => {
  const { server, port } = await startAgent();
  try {
    const method = await fetch(`http://127.0.0.1:${port}/models`, { method: "POST" });
    assert.equal(method.status, 405);
    const unknown = await fetch(`http://127.0.0.1:${port}/other`);
    assert.equal(unknown.status, 404);
    const options = await fetch(`http://127.0.0.1:${port}/models`, { method: "OPTIONS" });
    assert.equal(options.status, 204);
  } finally {
    await close(server);
  }
});

test("returns 503 when the workspace token is missing", async () => {
  const { server, port } = await startAgent({ token: undefined });
  try {
    const response = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, {
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
