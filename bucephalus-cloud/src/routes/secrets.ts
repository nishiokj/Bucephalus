import { authOwnerKey, type AuthContext } from "../auth";
import { HttpError, jsonResponse, readJsonObject, requireString } from "../http";
import { controlPlaneSecretNameViolation } from "../secrets/policy";
import type { CloudSecretRecord, CloudSecretRepository } from "../secrets/repository";
import { SECRET_NAME_PATTERN, type SecretStore } from "../secrets/store";

// GCP Secret Manager caps payloads at 64 KiB; enforcing the same bound on
// every backend keeps secrets portable across them.
const MAX_SECRET_VALUE_BYTES = 64 * 1024;

export async function handleSecretRoute(
  request: Request,
  url: URL,
  secrets: CloudSecretRepository,
  store: SecretStore,
  auth?: AuthContext | null,
): Promise<Response | null> {
  if (url.pathname !== "/v1/secrets" && !url.pathname.startsWith("/v1/secrets/")) {
    return null;
  }
  const ownerKey = authOwnerKey(auth);
  if (!ownerKey) {
    throw new HttpError(401, "unauthorized", "Secret management requires an authenticated user");
  }

  if (request.method === "GET" && url.pathname === "/v1/secrets") {
    const records = await secrets.listSecrets(ownerKey);
    return jsonResponse({ secrets: records.map(secretToWire) });
  }

  const name = secretNameFromPath(url.pathname);
  if (!name) {
    return null;
  }

  if (request.method === "PUT") {
    requireUsableSecretName(name);
    const body = await readJsonObject(request);
    const value = requireString(body.value, "/value");
    if (Buffer.byteLength(value, "utf8") > MAX_SECRET_VALUE_BYTES) {
      throw new HttpError(413, "secret_value_too_large", "Secret values are limited to 64 KiB", {
        max_secret_value_bytes: MAX_SECRET_VALUE_BYTES,
      });
    }
    const { storeName, backingRef } = await store.put(ownerKey, name, value);
    const { record, created } = await secrets.upsertSecret({ ownerKey, name, storeName, backingRef });
    return jsonResponse(secretToWire(record), { status: created ? 201 : 200 });
  }

  if (request.method === "DELETE") {
    const record = await secrets.deleteSecret(ownerKey, name);
    if (!record) {
      throw new HttpError(404, "secret_not_found", `No secret named '${name}'`);
    }
    await store.remove(record.store_name);
    return jsonResponse({ name: record.name, deleted: true });
  }

  return null;
}

// The wire shape is deliberately value-free: hosted secrets are write-only,
// so not even the backing provider ref leaves the control plane.
function secretToWire(record: CloudSecretRecord) {
  return {
    name: record.name,
    version: record.version,
    created_at: record.created_at,
    updated_at: record.updated_at,
  };
}

function secretNameFromPath(pathname: string): string | null {
  if (!pathname.startsWith("/v1/secrets/")) {
    return null;
  }
  const name = decodeURIComponent(pathname.slice("/v1/secrets/".length));
  return name.length > 0 && !name.includes("/") ? name : null;
}

function requireUsableSecretName(name: string): void {
  if (!SECRET_NAME_PATTERN.test(name)) {
    throw new HttpError(
      400,
      "invalid_secret_name",
      `Invalid secret name '${name}'. Use 1-128 characters of A-Z, a-z, 0-9, '_' or '-'.`,
    );
  }
  const violation = controlPlaneSecretNameViolation(name);
  if (violation) {
    throw new HttpError(400, "reserved_secret_name", violation);
  }
}
