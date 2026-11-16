import fs from "node:fs";
import http from "node:http";
import https from "node:https";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_PORT = 11233;
const DEFAULT_AI_BASE = "https://api.cnb.cool";
const DEFAULT_MODELS = [
  { id: "deepseek-v4-flash", object: "model", created: 0, owned_by: "cnb" },
  { id: "glm-5.3-flash", object: "model", created: 0, owned_by: "cnb" },
  { id: "kimi-k3", object: "model", created: 0, owned_by: "cnb" },
];

const HOP_BY_HOP = new Set([
  "connection",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
]);

const CORS_HEADERS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET,POST,OPTIONS",
  "access-control-allow-headers": "*",
  "access-control-max-age": "86400",
};

export function loadModels(modelsPath = path.join(HERE, "models.json")) {
  try {
    const parsed = JSON.parse(fs.readFileSync(modelsPath, "utf8"));
    if (Array.isArray(parsed?.models) && parsed.models.length > 0) {
      return parsed.models.filter((model) => typeof model?.id === "string");
    }
  } catch {
    // The default catalog keeps the agent usable when the file is missing.
  }
  return DEFAULT_MODELS.map((model) => ({ ...model }));
}

function sendJson(res, status, payload) {
  const body = Buffer.from(JSON.stringify(payload));
  res.writeHead(status, {
    ...CORS_HEADERS,
    "content-type": "application/json; charset=utf-8",
    "content-length": body.length,
  });
  res.end(body);
}

function sendError(res, status, message, type = "invalid_request_error", code = null) {
  sendJson(res, status, {
    error: { message, type, param: null, code },
  });
}

function normalizeRoute(pathname) {
  const trimmed = pathname.replace(/\/+$/, "") || "/";
  if (trimmed === "/v1") return "/";
  if (trimmed.startsWith("/v1/")) return trimmed.slice(3);
  return trimmed;
}

function copyHeaders(headers, { request }) {
  const result = {};
  for (const [name, value] of Object.entries(headers)) {
    const lower = name.toLowerCase();
    if (HOP_BY_HOP.has(lower)) continue;
    if (request && (lower === "host" || lower === "content-length")) continue;
    if (request && (lower === "authorization" || lower === "api-key" || lower === "x-api-key")) {
      continue;
    }
    result[lower] = value;
  }
  return result;
}

export function createAgentServer(options = {}) {
  const port = Number(options.port ?? DEFAULT_PORT);
  const token = options.token ?? process.env.CNB_TOKEN;
  const repoSlug = options.repoSlug ?? process.env.CNB_REPO_SLUG;
  const aiBase = (options.aiBase ?? process.env.CNB_AI_BASE ?? DEFAULT_AI_BASE).replace(/\/+$/, "");
  const models = options.models ?? loadModels(options.modelsPath);

  return http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://127.0.0.1");
    const route = normalizeRoute(url.pathname);

    if (req.method === "OPTIONS") {
      res.writeHead(204, CORS_HEADERS);
      res.end();
      return;
    }

    if (route === "/healthz") {
      if (req.method !== "GET") {
        sendError(res, 405, "method not allowed", "invalid_request_error", "method_not_allowed");
        return;
      }
      sendJson(res, 200, {
        status: "ok",
        version: "0.1.0",
        uptime: Math.round(process.uptime()),
      });
      return;
    }

    if (route === "/models") {
      if (req.method !== "GET") {
        sendError(res, 405, "method not allowed", "invalid_request_error", "method_not_allowed");
        return;
      }
      sendJson(res, 200, { object: "list", data: models });
      return;
    }

    if (route.startsWith("/models/")) {
      if (req.method !== "GET") {
        sendError(res, 405, "method not allowed", "invalid_request_error", "method_not_allowed");
        return;
      }
      let id;
      try {
        id = decodeURIComponent(route.slice("/models/".length));
      } catch {
        sendError(res, 400, "invalid model id");
        return;
      }
      const model = models.find((item) => item.id === id);
      if (!model) {
        sendError(
          res,
          404,
          `The model '${id}' does not exist`,
          "invalid_request_error",
          "model_not_found",
        );
        return;
      }
      sendJson(res, 200, model);
      return;
    }

    if (route === "/chat/completions") {
      if (req.method !== "POST") {
        sendError(res, 405, "method not allowed", "invalid_request_error", "method_not_allowed");
        return;
      }
      if (!token) {
        sendError(res, 503, "workspace token is missing", "service_unavailable", "token_missing");
        return;
      }
      if (!repoSlug) {
        sendError(res, 503, "workspace repository is missing", "service_unavailable", "repo_missing");
        return;
      }

      const upstreamUrl = new URL(
        `/${repoSlug}/-/ai/chat/completions${url.search}`,
        aiBase,
      );
      const headers = copyHeaders(req.headers, { request: true });
      headers.authorization = `Bearer ${token}`;
      if (!headers["content-type"]) headers["content-type"] = "application/json";

      const transport = upstreamUrl.protocol === "https:" ? https : http;
      const upstream = transport.request(
        upstreamUrl,
        { method: "POST", headers, timeout: 10_000 },
        (upstreamResponse) => {
          upstream.setTimeout(0);
          res.writeHead(
            upstreamResponse.statusCode ?? 502,
            copyHeaders(upstreamResponse.headers, { request: false }),
          );
          res.flushHeaders?.();
          res.socket?.setNoDelay?.(true);
          upstreamResponse.pipe(res);
        },
      );

      upstream.on("timeout", () => {
        upstream.destroy(new Error("upstream connect timeout"));
      });
      upstream.on("error", (error) => {
        if (res.headersSent) {
          res.destroy();
        } else {
          sendError(res, 502, `upstream error: ${error.message}`, "upstream_error");
        }
      });
      req.on("aborted", () => upstream.destroy());
      res.on("close", () => {
        if (!res.writableEnded) upstream.destroy();
      });
      req.pipe(upstream);
      return;
    }

    sendError(res, 404, "not found", "invalid_request_error", "not_found");
  });
}

function parseArgs(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    const name = argv[index];
    if ((name === "--port" || name === "--models") && index + 1 < argv.length) {
      result[name.slice(2)] = argv[index + 1];
      index += 1;
    }
  }
  return result;
}

const isMain =
  process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;

if (isMain) {
  const args = parseArgs(process.argv.slice(2));
  const port = Number(args.port ?? DEFAULT_PORT);
  const server = createAgentServer({
    port,
    modelsPath: args.models ? path.resolve(args.models) : undefined,
  });
  server.listen(port, "127.0.0.1", () => {
    console.log(`cnb-ai-bridge agent listening on 127.0.0.1:${port}`);
  });
}
