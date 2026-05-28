import { loadConfig } from "./config";
import { checkDatabase, createSql } from "./db/client";
import { errorResponse, jsonResponse } from "./http";
import { ImportRepository } from "./imports/repository";
import { RegistryRepository } from "./registry/repository";
import { handleDraftRoute } from "./routes/drafts";
import { handleImportRoute } from "./routes/imports";
import { handleRegistryRoute } from "./routes/registry";

const config = loadConfig();
const sql = createSql(config.databaseUrl);
const registry = new RegistryRepository(sql);
const imports = new ImportRepository(sql);

const server = Bun.serve({
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

      const importResponse = await handleImportRoute(request, url, imports, registry);
      if (importResponse) {
        return importResponse;
      }

      return jsonResponse({ code: "not_found", message: "Route not found" }, { status: 404 });
    } catch (error) {
      return errorResponse(error);
    }
  },
});

console.log(`bucephalus-cloud api listening on http://localhost:${server.port}`);

process.on("SIGINT", async () => {
  await sql.end({ timeout: 1 });
  process.exit(0);
});

process.on("SIGTERM", async () => {
  await sql.end({ timeout: 1 });
  process.exit(0);
});
