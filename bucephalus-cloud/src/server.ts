import { loadConfig } from "./config";
import { OAuthVerifier, type AuthContext } from "./auth";
import { checkDatabase, createSql } from "./db/client";
import { errorResponse, HttpError, jsonResponse } from "./http";
import { ImportRepository } from "./imports/repository";
import { LatchSubmissionRepository } from "./latch/repository";
import { PackageRepository, RunRepository } from "./packages/repository";
import { RegistryRepository } from "./registry/repository";
import { RuntimeRepository } from "./runtime/repository";
import { RunnerRepository } from "./runners/repository";
import { handleDraftRoute } from "./routes/drafts";
import { handleImportRoute } from "./routes/imports";
import { handleLatchRoute } from "./routes/latch";
import { handleRegistryRoute } from "./routes/registry";
import { handleRunnerRoute } from "./routes/runners";
import { handleRunRoute } from "./routes/runs";
import {
  headersForTrace,
  initTelemetry,
  logError,
  logInfo,
  newTraceContext,
  parseTraceContextFromHeaders,
  type TraceContext,
} from "./logging";

await initTelemetry();

const config = loadConfig();
const workerToken = config.workerToken;
if (!workerToken) {
  throw new Error("BUCEPHALUS_CLOUD_WORKER_TOKEN is required for worker and runner management routes");
}
const runnerAdminToken = config.runnerAdminToken ?? workerToken;
const sql = createSql(config.databaseUrl);
const registry = new RegistryRepository(sql);
const imports = new ImportRepository(sql);
const latchSubmissions = new LatchSubmissionRepository(sql);
const packages = new PackageRepository(sql);
const runs = new RunRepository(sql);
const runtime = new RuntimeRepository(sql, process.env.BUCEPHALUS_RUN_STORE_SCHEMA);
const runners = new RunnerRepository(sql);
const auth = new OAuthVerifier(config.auth);

const server = Bun.serve({
  hostname: config.host,
  port: config.port,
  async fetch(request) {
    const url = new URL(request.url);
    const traceContext = parseTraceContextFromHeaders(request.headers, "api");
    const startedAt = performance.now();
    try {
      if (request.method === "OPTIONS") {
        const response = withCors(new Response(null, { status: 204 }), traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      if (request.method === "GET" && url.pathname === "/healthz") {
        const response = withCors(jsonResponse({ ok: true }), traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      if (request.method === "GET" && url.pathname === "/readyz") {
        await checkDatabase(sql);
        const response = withCors(jsonResponse({ ok: true, database: "ok" }), traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      const userAuth: AuthContext | null = requiresUserAuth(url.pathname)
        ? await auth.requireUser(request, "Bucephalus Cloud")
        : null;

      const registryResponse = await handleRegistryRoute(request, url, registry);
      if (registryResponse) {
        const response = withCors(registryResponse, traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      const draftResponse = await handleDraftRoute(request, url, registry);
      if (draftResponse) {
        const response = withCors(draftResponse, traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      const importResponse = await handleImportRoute(request, url, imports, packages, userAuth);
      if (importResponse) {
        const response = withCors(importResponse, traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      const latchResponse = await handleLatchRoute(request, url, registry, latchSubmissions, userAuth);
      if (latchResponse) {
        const response = withCors(latchResponse, traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      const runnerResponse = await handleRunnerRoute(request, url, runners, {
        workerToken,
        adminToken: runnerAdminToken,
      });
      if (runnerResponse) {
        const response = withCors(runnerResponse, traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      const runResponse = await handleRunRoute(request, url, packages, runs, runtime, workerToken, userAuth);
      if (runResponse) {
        const response = withCors(runResponse, traceContext);
        logInfo("http.request.completed", traceContext, {
          method: request.method,
          path: url.pathname,
          status: response.status,
          latency_ms: Math.round(performance.now() - startedAt),
        });
        return response;
      }

      const response = withCors(jsonResponse({ code: "not_found", message: "Route not found" }, { status: 404 }), traceContext);
      logInfo("http.request.completed", traceContext, {
        method: request.method,
        path: url.pathname,
        status: response.status,
        latency_ms: Math.round(performance.now() - startedAt),
      });
      return response;
    } catch (error) {
      const errorResponseValue = errorResponse(error);
      const status = errorResponseValue.status;
      const code = error instanceof HttpError ? error.code : "internal_error";
      const detail = error instanceof HttpError ? error.message : "Internal server error";
      logError("http.request.failed", traceContext, {
        method: request.method,
        path: url.pathname,
        status,
        code,
        error: detail,
        latency_ms: Math.round(performance.now() - startedAt),
      });
      return withCors(errorResponseValue, traceContext);
    }
  },
});

const serviceContext = newTraceContext({
  component: "api",
  requestId: `${config.host}:${config.port}`,
});

logInfo("api.starting", serviceContext, {
  host: config.host,
  port: server.port,
});
logInfo("api.startup", serviceContext, {
  user_oauth_required: true,
});

process.on("SIGINT", async () => {
  logInfo("api.shutdown_requested", serviceContext, { signal: "SIGINT" });
  await sql.end({ timeout: 1 });
  process.exit(0);
});

process.on("SIGTERM", async () => {
  logInfo("api.shutdown_requested", serviceContext, { signal: "SIGTERM" });
  await sql.end({ timeout: 1 });
  process.exit(0);
});

function withCors(response: Response, traceContext: TraceContext): Response {
  const headers = new Headers(response.headers);
  headers.set("access-control-allow-origin", "*");
  headers.set("access-control-allow-methods", "GET,POST,PUT,OPTIONS");
  headers.set(
    "access-control-allow-headers",
    "authorization,content-type,x-request-id,x-bucephalus-request-id,x-bucephalus-trace-id,x-bucephalus-span-id,x-bucephalus-parent-span-id",
  );
  headers.set(
    "access-control-expose-headers",
    "x-request-id,x-bucephalus-request-id,x-bucephalus-trace-id,x-bucephalus-span-id,x-bucephalus-parent-span-id",
  );
  headers.set("access-control-max-age", "600");
  for (const [key, value] of Object.entries(headersForTrace(traceContext))) {
    headers.set(key, value);
  }
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

function requiresUserAuth(pathname: string): boolean {
  if (pathname.startsWith("/v1/worker/") || pathname.startsWith("/v1/runner-")) {
    return false;
  }
  if (pathname.startsWith("/v1/packages/") && pathname.endsWith("/content")) {
    return false;
  }
  return pathname.startsWith("/v1/");
}
