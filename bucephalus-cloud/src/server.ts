import { loadConfig } from "./config";
import { OAuthVerifier, type AuthContext } from "./auth";
import { checkDatabase, createSql } from "./db/client";
import { errorResponse, jsonResponse } from "./http";
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
    try {
      if (request.method === "OPTIONS") {
        return withCors(new Response(null, { status: 204 }));
      }

      if (request.method === "GET" && url.pathname === "/healthz") {
        return withCors(jsonResponse({ ok: true }));
      }

      if (request.method === "GET" && url.pathname === "/readyz") {
        await checkDatabase(sql);
        return withCors(jsonResponse({ ok: true, database: "ok" }));
      }

      const userAuth: AuthContext | null = requiresUserAuth(url.pathname)
        ? await auth.requireUser(request, "Bucephalus Cloud")
        : null;

      const registryResponse = await handleRegistryRoute(request, url, registry);
      if (registryResponse) {
        return withCors(registryResponse);
      }

      const draftResponse = await handleDraftRoute(request, url, registry);
      if (draftResponse) {
        return withCors(draftResponse);
      }

      const importResponse = await handleImportRoute(request, url, imports, packages, userAuth);
      if (importResponse) {
        return withCors(importResponse);
      }

      const latchResponse = await handleLatchRoute(request, url, registry, latchSubmissions, userAuth);
      if (latchResponse) {
        return withCors(latchResponse);
      }

      const runnerResponse = await handleRunnerRoute(request, url, runners, {
        workerToken,
        adminToken: runnerAdminToken,
      });
      if (runnerResponse) {
        return withCors(runnerResponse);
      }

      const runResponse = await handleRunRoute(request, url, packages, runs, runtime, workerToken, userAuth);
      if (runResponse) {
        return withCors(runResponse);
      }

      return withCors(jsonResponse({ code: "not_found", message: "Route not found" }, { status: 404 }));
    } catch (error) {
      return withCors(errorResponse(error));
    }
  },
});

console.log(`bucephalus-cloud api listening on http://${config.host}:${server.port}`);
console.log(`user_oauth=${config.auth.required ? "required" : "disabled"}`);

process.on("SIGINT", async () => {
  await sql.end({ timeout: 1 });
  process.exit(0);
});

process.on("SIGTERM", async () => {
  await sql.end({ timeout: 1 });
  process.exit(0);
});

function withCors(response: Response): Response {
  const headers = new Headers(response.headers);
  headers.set("access-control-allow-origin", "*");
  headers.set("access-control-allow-methods", "GET,POST,PUT,OPTIONS");
  headers.set("access-control-allow-headers", "authorization,content-type");
  headers.set("access-control-max-age", "600");
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
