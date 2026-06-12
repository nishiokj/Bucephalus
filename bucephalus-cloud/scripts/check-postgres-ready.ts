import postgres from "postgres";
import { connect } from "node:net";

const databaseUrl = process.env.BUCEPHALUS_CLOUD_HTTP_WORKFLOW_DATABASE_URL
  ?? process.env.BUCEPHALUS_MIGRATION_TEST_DATABASE_URL
  ?? process.env.DATABASE_URL
  ?? "";

if (!databaseUrl.trim()) {
  fail("DATABASE_URL, BUCEPHALUS_MIGRATION_TEST_DATABASE_URL, or BUCEPHALUS_CLOUD_HTTP_WORKFLOW_DATABASE_URL is required");
}

const timeoutMs = positiveInteger(process.env.BUCEPHALUS_CLOUD_POSTGRES_READY_TIMEOUT_MS, 7_000);

let parsed: URL;
try {
  parsed = new URL(databaseUrl);
} catch {
  fail("Postgres readiness check received an invalid database URL");
}

if (parsed.protocol !== "postgres:" && parsed.protocol !== "postgresql:") {
  fail(`Postgres readiness check requires a postgres URL, got ${parsed.protocol}`);
}

const sql = postgres(databaseUrl, {
  max: 1,
  idle_timeout: 1,
  connect_timeout: Math.max(1, Math.ceil(timeoutMs / 1000)),
});

try {
  await probeTcp(parsed, Math.min(timeoutMs, 2_000));
  await withTimeout(sql`select 1 as ok`, timeoutMs);
  console.log(`Postgres ready: ${redactedDatabaseTarget(parsed)}`);
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  fail(
    `Postgres readiness check failed for ${redactedDatabaseTarget(parsed)} after ${timeoutMs}ms: ${message}\n`
      + "Start the local/CI Cloud Postgres service or set DATABASE_URL to a reachable local/CI Postgres URL."
      + localhostTimeoutHint(parsed, message),
  );
} finally {
  await sql.end({ timeout: 1 }).catch(() => {});
}

function probeTcp(url: URL, timeoutMs: number): Promise<void> {
  const port = Number.parseInt(url.port || "5432", 10);
  const host = url.hostname.replace(/^\[|\]$/g, "");
  return new Promise((resolve, reject) => {
    const socket = connect({ host, port });
    const timeout = setTimeout(() => {
      socket.destroy();
      reject(new Error(`TCP connection to ${host}:${port} timed out before Postgres handshake`));
    }, timeoutMs);
    timeout.unref?.();
    socket.once("connect", () => {
      clearTimeout(timeout);
      socket.end();
      resolve();
    });
    socket.once("error", (error) => {
      clearTimeout(timeout);
      reject(new Error(`TCP connection to ${host}:${port} failed: ${error.message}`));
    });
  });
}

function withTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      reject(new Error("TCP connection succeeded, but Postgres handshake/query timed out"));
    }, timeoutMs);
    timeout.unref?.();
    promise.then(
      (value) => {
        clearTimeout(timeout);
        resolve(value);
      },
      (error) => {
        clearTimeout(timeout);
        reject(error);
      },
    );
  });
}

function redactedDatabaseTarget(url: URL): string {
  const port = url.port ? `:${url.port}` : "";
  const database = url.pathname && url.pathname !== "/" ? url.pathname : "/(default)";
  return `${url.protocol}//${url.hostname}${port}${database}`;
}

function localhostTimeoutHint(url: URL, message: string): string {
  if (url.hostname.toLowerCase() !== "localhost" || !message.toLowerCase().includes("timed out")) {
    return "";
  }
  const fallback = new URL(url.toString());
  fallback.hostname = "127.0.0.1";
  return `\nIf localhost is routed through a stale IPv6 or VM forwarder, retry ${redactedDatabaseTarget(fallback)}.`;
}

function positiveInteger(raw: string | undefined, fallback: number): number {
  const parsed = Number.parseInt(raw ?? "", 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function fail(message: string): never {
  console.error(message);
  process.exit(1);
}
