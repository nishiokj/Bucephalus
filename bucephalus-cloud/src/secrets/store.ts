import { createHash } from "node:crypto";
import { mkdir, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import type { AppConfig } from "../config";

export const SECRET_NAME_PATTERN = /^[A-Za-z0-9_-]{1,128}$/;

type Fetcher = (input: string | URL | Request, init?: RequestInit) => Promise<Response>;

export interface SecretStoreBackend {
  write(storeName: string, value: string): Promise<{ backingRef: string }>;
  remove(storeName: string): Promise<void>;
}

// Hosted secrets are write-only through the Cloud API: the control plane
// stores values under an opaque per-owner store name and hands workers a
// provider ref the existing attempt-scoped secret resolver already speaks.
// Plaintext is never persisted in Postgres and never readable back out.
export class SecretStore {
  constructor(
    private readonly backend: SecretStoreBackend,
    private readonly prefix: string,
  ) {}

  async put(ownerKey: string, name: string, value: string): Promise<{ storeName: string; backingRef: string }> {
    const storeName = storeSecretName(this.prefix, ownerKey, name);
    const { backingRef } = await this.backend.write(storeName, value);
    return { storeName, backingRef };
  }

  async remove(storeName: string): Promise<void> {
    await this.backend.remove(storeName);
  }
}

// GCP secret ids cap at 255 chars of [A-Za-z0-9_-]; the owner hash keeps
// distinct owners collision-free without leaking the issuer:subject key.
export function storeSecretName(prefix: string, ownerKey: string, name: string): string {
  const ownerHash = createHash("sha256").update(ownerKey).digest("hex").slice(0, 16);
  return `${prefix}-${ownerHash}-${name}`;
}

export function createSecretStoreBackend(config: AppConfig, fetchImpl?: Fetcher): SecretStoreBackend {
  if (config.secrets.backend === "gcp") {
    return new GcpSecretStoreBackend(config.secrets.project, fetchImpl);
  }
  return new FilesystemSecretStoreBackend(config.dataDir);
}

export class GcpSecretStoreBackend implements SecretStoreBackend {
  constructor(
    private readonly project: string,
    private readonly fetchImpl: Fetcher = fetch,
  ) {}

  async write(storeName: string, value: string): Promise<{ backingRef: string }> {
    const token = await this.accessToken();
    const headers = {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
    };
    const createResponse = await this.fetchImpl(
      `${this.secretsUrl()}?secretId=${encodeURIComponent(storeName)}`,
      {
        method: "POST",
        headers,
        body: JSON.stringify({ replication: { automatic: {} } }),
      },
    );
    if (!createResponse.ok && createResponse.status !== 409) {
      throw new Error(`GCP Secret Manager create failed with HTTP ${createResponse.status}: ${await safeBody(createResponse)}`);
    }
    const addResponse = await this.fetchImpl(
      `${this.secretsUrl()}/${encodeURIComponent(storeName)}:addVersion`,
      {
        method: "POST",
        headers,
        body: JSON.stringify({ payload: { data: Buffer.from(value, "utf8").toString("base64") } }),
      },
    );
    if (!addResponse.ok) {
      throw new Error(`GCP Secret Manager addVersion failed with HTTP ${addResponse.status}: ${await safeBody(addResponse)}`);
    }
    const payload = await addResponse.json() as { name?: unknown };
    const version = typeof payload.name === "string" ? payload.name.split("/versions/")[1] : undefined;
    if (!version || !/^[0-9]+$/.test(version)) {
      throw new Error("GCP Secret Manager addVersion response did not include a version name");
    }
    return {
      backingRef: `gcp-secret-manager://projects/${this.project}/secrets/${storeName}/versions/${version}`,
    };
  }

  async remove(storeName: string): Promise<void> {
    const token = await this.accessToken();
    const response = await this.fetchImpl(
      `${this.secretsUrl()}/${encodeURIComponent(storeName)}`,
      {
        method: "DELETE",
        headers: { authorization: `Bearer ${token}` },
      },
    );
    if (!response.ok && response.status !== 404) {
      throw new Error(`GCP Secret Manager delete failed with HTTP ${response.status}: ${await safeBody(response)}`);
    }
  }

  private secretsUrl(): string {
    return `https://secretmanager.googleapis.com/v1/projects/${encodeURIComponent(this.project)}/secrets`;
  }

  private async accessToken(): Promise<string> {
    const response = await this.fetchImpl(
      "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token",
      {
        headers: { "Metadata-Flavor": "Google" },
      },
    );
    if (!response.ok) {
      throw new Error(`GCP metadata token request failed with HTTP ${response.status}`);
    }
    const body = await response.json() as { access_token?: unknown };
    if (typeof body.access_token !== "string" || body.access_token.length === 0) {
      throw new Error("GCP metadata token response did not include an access_token");
    }
    return body.access_token;
  }
}

// Local development backend: values land on the API host's disk and resolve
// through the resolver's explicitly enabled file: scheme. Store names are
// validated to SECRET_NAME_PATTERN-safe characters upstream, so they cannot
// escape the secrets directory.
export class FilesystemSecretStoreBackend implements SecretStoreBackend {
  constructor(private readonly dataDir: string) {}

  async write(storeName: string, value: string): Promise<{ backingRef: string }> {
    const dir = resolve(join(this.dataDir, "secrets"));
    await mkdir(dir, { recursive: true, mode: 0o700 });
    const path = join(dir, storeName);
    await writeFile(path, value, { mode: 0o600 });
    return { backingRef: `file:${path}` };
  }

  async remove(storeName: string): Promise<void> {
    await rm(join(resolve(join(this.dataDir, "secrets")), storeName), { force: true });
  }
}

async function safeBody(response: Response): Promise<string> {
  try {
    return (await response.text()).slice(0, 1000);
  } catch {
    return "<unreadable body>";
  }
}
