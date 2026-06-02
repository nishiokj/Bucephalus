import { readFile } from "node:fs/promises";
import { join } from "node:path";

const root = import.meta.dir;
const dist = join(root, "dist");
const port = Number.parseInt(process.env.BUCEPHALUS_WEB_PORT ?? "5174", 10);
const host = process.env.BUCEPHALUS_WEB_HOST ?? "127.0.0.1";
const apiBase = process.env.BUCEPHALUS_CLOUD_API_URL ?? "http://100.86.188.117:8099";
const userToken = process.env.BUCEPHALUS_WEB_DEFAULT_USER_TOKEN ?? "";
const workerToken = process.env.BUCEPHALUS_WEB_DEFAULT_WORKER_TOKEN ?? "";
const runnerPoolId = process.env.BUCEPHALUS_WEB_RUNNER_POOL_ID ?? process.env.BUCEPHALUS_POOL_CONTROLLER_POOL_ID ?? "";

await build();

Bun.serve({
  hostname: host,
  port,
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname === "/config.js") {
      return new Response(
        `window.BUCEPHALUS_WEB_CONFIG = ${JSON.stringify({ apiBase, userToken, workerToken, runnerPoolId })};\n`,
        { headers: { "content-type": "application/javascript; charset=utf-8" } },
      );
    }
    if (url.pathname === "/assets/app.js") {
      await build();
      return fileResponse(join(dist, "app.js"), "application/javascript; charset=utf-8");
    }
    if (url.pathname === "/" || url.pathname === "/index.html") {
      return fileResponse(join(root, "index.html"), "text/html; charset=utf-8");
    }
    if (url.pathname === "/styles.css") {
      return fileResponse(join(root, "styles.css"), "text/css; charset=utf-8");
    }
    return new Response("not found", { status: 404 });
  },
});

console.log(`bucephalus cloud console listening on http://${host}:${port}`);
console.log(`api_base=${apiBase}`);
console.log(`user_token_default=${userToken ? "set" : "unset"}`);
console.log(`worker_token_default=${workerToken ? "set" : "unset"}`);
console.log(`runner_pool_id=${runnerPoolId || "unset"}`);

async function build(): Promise<void> {
  const result = await Bun.build({
    entrypoints: [join(root, "src", "app.ts")],
    outdir: dist,
    target: "browser",
    sourcemap: "inline",
  });
  if (!result.success) {
    throw new Error(result.logs.map((log) => String(log)).join("\n"));
  }
}

async function fileResponse(path: string, contentType: string): Promise<Response> {
  return new Response(await readFile(path), {
    headers: {
      "content-type": contentType,
      "cache-control": "no-store",
    },
  });
}
