import { loadConfig } from "./config";
import { checkDatabase, createSql } from "./db/client";
import { errorResponse, jsonResponse } from "./http";
import { ImportRepository } from "./imports/repository";
import { PackageRepository, RunRepository } from "./packages/repository";
import { RegistryRepository } from "./registry/repository";
import { RunnerRepository } from "./runners/repository";
import { handleDraftRoute } from "./routes/drafts";
import { handleImportRoute } from "./routes/imports";
import { handleRegistryRoute } from "./routes/registry";
import { handleRunnerRoute } from "./routes/runners";
import { handleRunRoute } from "./routes/runs";

const config = loadConfig();
const workerToken = config.workerToken;
if (!workerToken) {
  throw new Error("BUCEPHALUS_CLOUD_WORKER_TOKEN is required for worker and runner management routes");
}
const sql = createSql(config.databaseUrl);
const registry = new RegistryRepository(sql);
const imports = new ImportRepository(sql);
const packages = new PackageRepository(sql);
const runs = new RunRepository(sql);
const runners = new RunnerRepository(sql);

const server = Bun.serve({
  hostname: config.host,
  port: config.port,
  async fetch(request) {
    const url = new URL(request.url);
    try {
      if (request.method === "GET" && url.pathname === "/healthz") {
        return jsonResponse({ ok: true });
      }

      if (request.method === "GET" && url.pathname === "/readyz") {
        await checkDatabase(sql);
        return jsonResponse({ ok: true, database: "ok" });
      }

      const registryResponse = await handleRegistryRoute(request, url, registry);
      if (registryResponse) {
        return registryResponse;
      }

      const draftResponse = await handleDraftRoute(request, url, registry);
      if (draftResponse) {
        return draftResponse;
      }

      const importResponse = await handleImportRoute(request, url, imports, packages);
      if (importResponse) {
        return importResponse;
      }

      const runnerResponse = await handleRunnerRoute(request, url, runners, workerToken);
      if (runnerResponse) {
        return runnerResponse;
      }

      const runResponse = await handleRunRoute(request, url, packages, runs, workerToken);
      if (runResponse) {
        return runResponse;
      }

      return jsonResponse({ code: "not_found", message: "Route not found" }, { status: 404 });
    } catch (error) {
      return errorResponse(error);
    }
  },
});

console.log(`bucephalus-cloud api listening on http://${config.host}:${server.port}`);

process.on("SIGINT", async () => {
  await sql.end({ timeout: 1 });
  process.exit(0);
});

process.on("SIGTERM", async () => {
  await sql.end({ timeout: 1 });
  process.exit(0);
});
