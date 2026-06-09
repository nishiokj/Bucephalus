import { createHash, createHmac } from "node:crypto";
import { mkdir, readFile, realpath, writeFile } from "node:fs/promises";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { loadConfig, type AppConfig } from "./config";

const EMPTY_SHA256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

export class ObjectStorageBoundaryError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ObjectStorageBoundaryError";
  }
}

export function isObjectStorageBoundaryError(error: unknown): error is ObjectStorageBoundaryError {
  return error instanceof ObjectStorageBoundaryError;
}

export async function putUploadObject(
  uploadId: string,
  bytes: Uint8Array,
  mediaType: string,
  config: AppConfig = loadConfig(),
): Promise<string> {
  if (config.storage.backend === "filesystem") {
    const uploadDir = join(config.dataDir, "uploads", safeObjectSegment(uploadId));
    await mkdir(uploadDir, { recursive: true });
    const storagePath = join(uploadDir, "content.blob");
    await writeFile(storagePath, bytes);
    return storagePath;
  }
  if (config.storage.backend === "gcs") {
    const key = objectKey(config.storage.prefix, "uploads", safeObjectSegment(uploadId), "content.blob");
    await gcsRequest({
      method: "PUT",
      bucket: config.storage.bucket,
      key,
      body: bytes,
      contentType: mediaType,
      config,
    });
    return gcsUri(config.storage.bucket, key);
  }

  const key = objectKey(config.storage.prefix, "uploads", safeObjectSegment(uploadId), "content.blob");
  await r2Request({
    method: "PUT",
    bucket: config.storage.bucket,
    key,
    body: bytes,
    contentType: mediaType,
    config,
  });
  return r2Uri(config.storage.bucket, key);
}

export async function readStoredObject(storagePath: string, config: AppConfig = loadConfig()): Promise<Uint8Array> {
  const r2Object = parseR2Uri(storagePath);
  const gcsObject = parseGcsUri(storagePath);
  if (config.storage.backend === "filesystem") {
    if (r2Object || gcsObject) {
      throw storageBoundaryError("Stored object path does not match configured filesystem storage");
    }
    return new Uint8Array(await readFile(await verifiedFilesystemObjectPath(storagePath, config)));
  }
  if (config.storage.backend === "gcs") {
    if (!gcsObject) {
      throw storageBoundaryError("Stored object path does not match configured GCS storage");
    }
    assertGcsObjectBelongsToConfig(gcsObject, config.storage);
    const response = await gcsRequest({
      method: "GET",
      bucket: gcsObject.bucket,
      key: gcsObject.key,
      config,
    });
    return new Uint8Array(await response.arrayBuffer());
  }

  if (!r2Object) {
    throw storageBoundaryError("Stored object path does not match configured R2 storage");
  }
  assertR2ObjectBelongsToConfig(r2Object, config.storage);
  const response = await r2Request({
    method: "GET",
    bucket: r2Object.bucket,
    key: r2Object.key,
    config,
  });
  return new Uint8Array(await response.arrayBuffer());
}

export async function materializeStoredObject(
  storagePath: string,
  workDir: string,
  filename: string,
  config: AppConfig = loadConfig(),
): Promise<string> {
  if (config.storage.backend === "filesystem") {
    if (parseR2Uri(storagePath) || parseGcsUri(storagePath)) {
      throw storageBoundaryError("Stored object path does not match configured filesystem storage");
    }
    return await verifiedFilesystemObjectPath(storagePath, config);
  }
  if (config.storage.backend === "r2" && !parseR2Uri(storagePath)) {
    throw storageBoundaryError("Stored object path does not match configured R2 storage");
  }
  if (config.storage.backend === "gcs" && !parseGcsUri(storagePath)) {
    throw storageBoundaryError("Stored object path does not match configured GCS storage");
  }
  await mkdir(workDir, { recursive: true });
  const localPath = join(workDir, filename);
  await writeFile(localPath, await readStoredObject(storagePath, config));
  return localPath;
}

function parseR2Uri(storagePath: string): { bucket: string; key: string } | null {
  if (!storagePath.startsWith("r2://")) {
    return null;
  }
  let url: URL;
  try {
    url = new URL(storagePath);
  } catch {
    throw storageBoundaryError("Stored object path is not a valid R2 URI");
  }
  return {
    bucket: url.hostname,
    key: decodeURIComponent(url.pathname.replace(/^\/+/, "")),
  };
}

function parseGcsUri(storagePath: string): { bucket: string; key: string } | null {
  if (!storagePath.startsWith("gcs://") && !storagePath.startsWith("gs://")) {
    return null;
  }
  let url: URL;
  try {
    url = new URL(storagePath.replace(/^gs:\/\//, "gcs://"));
  } catch {
    throw storageBoundaryError("Stored object path is not a valid GCS URI");
  }
  return {
    bucket: url.hostname,
    key: decodeURIComponent(url.pathname.replace(/^\/+/, "")),
  };
}

async function verifiedFilesystemObjectPath(storagePath: string, config: AppConfig): Promise<string> {
  const uploadRoot = resolve(config.dataDir, "uploads");
  const candidate = resolve(storagePath);
  const rawRelative = relative(uploadRoot, candidate);
  if (!isExpectedUploadObjectRelativePath(rawRelative)) {
    throw storageBoundaryError("Stored object path does not match configured filesystem upload storage");
  }

  let realUploadRoot: string;
  let realCandidate: string;
  try {
    realUploadRoot = await realpath(uploadRoot);
    realCandidate = await realpath(candidate);
  } catch {
    throw storageBoundaryError("Stored object is unavailable");
  }

  const realRelative = relative(realUploadRoot, realCandidate);
  if (realRelative.startsWith("..") || isAbsolute(realRelative)) {
    throw storageBoundaryError("Stored object path resolves outside configured filesystem upload storage");
  }
  return candidate;
}

function assertR2ObjectBelongsToConfig(
  r2Object: { bucket: string; key: string },
  storage: Extract<AppConfig["storage"], { backend: "r2" }>,
): void {
  if (r2Object.bucket !== storage.bucket) {
    throw storageBoundaryError("Stored object path does not match configured R2 bucket");
  }
  const uploadsPrefix = objectKey(storage.prefix, "uploads");
  const relativeKey = r2Object.key.startsWith(`${uploadsPrefix}/`)
    ? r2Object.key.slice(uploadsPrefix.length + 1)
    : "";
  if (!isExpectedUploadObjectRelativePath(relativeKey, "/")) {
    throw storageBoundaryError("Stored object path does not match configured R2 upload storage");
  }
}

function assertGcsObjectBelongsToConfig(
  gcsObject: { bucket: string; key: string },
  storage: Extract<AppConfig["storage"], { backend: "gcs" }>,
): void {
  if (gcsObject.bucket !== storage.bucket) {
    throw storageBoundaryError("Stored object path does not match configured GCS bucket");
  }
  const uploadsPrefix = objectKey(storage.prefix, "uploads");
  const relativeKey = gcsObject.key.startsWith(`${uploadsPrefix}/`)
    ? gcsObject.key.slice(uploadsPrefix.length + 1)
    : "";
  if (!isExpectedUploadObjectRelativePath(relativeKey, "/")) {
    throw storageBoundaryError("Stored object path does not match configured GCS upload storage");
  }
}

function isExpectedUploadObjectRelativePath(value: string, separator = sep): boolean {
  if (!value || value.startsWith("..") || isAbsolute(value)) {
    return false;
  }
  const parts = value.split(separator);
  return parts.length === 2 && (parts[0]?.length ?? 0) > 0 && parts[1] === "content.blob";
}

function storageBoundaryError(message: string): Error {
  return new ObjectStorageBoundaryError(message);
}

function objectKey(prefix: string, ...parts: string[]): string {
  return [prefix, ...parts].filter(Boolean).join("/");
}

function r2Uri(bucket: string, key: string): string {
  return `r2://${bucket}/${key.split("/").map(encodePathSegment).join("/")}`;
}

function gcsUri(bucket: string, key: string): string {
  return `gcs://${bucket}/${key.split("/").map(encodePathSegment).join("/")}`;
}

function safeObjectSegment(value: string): string {
  return encodePathSegment(value);
}

async function r2Request(input: {
  method: "GET" | "PUT";
  bucket: string;
  key: string;
  config: AppConfig;
  body?: Uint8Array;
  contentType?: string;
}): Promise<Response> {
  if (input.config.storage.backend !== "r2") {
    throw new Error("R2 object path requires R2 storage configuration");
  }
  const endpoint = new URL(input.config.storage.endpoint);
  const canonicalPath = `/${[input.bucket, ...input.key.split("/")].map(encodePathSegment).join("/")}`;
  const url = `${endpoint.origin}${canonicalPath}`;
  const body = input.body ?? new Uint8Array();
  const payloadHash = input.method === "GET" ? EMPTY_SHA256 : sha256Hex(body);
  const now = new Date();
  const headers = signedR2Headers({
    method: input.method,
    host: endpoint.host,
    canonicalPath,
    payloadHash,
    contentType: input.contentType,
    now,
    accessKeyId: input.config.storage.accessKeyId,
    secretAccessKey: input.config.storage.secretAccessKey,
  });
  const init: RequestInit = {
    method: input.method,
    headers,
  };
  if (input.method === "PUT") {
    init.body = toArrayBuffer(body);
  }
  const response = await fetch(url, init);
  if (!response.ok) {
    throw new Error(`R2 ${input.method} ${input.bucket}/${input.key} failed with HTTP ${response.status}`);
  }
  return response;
}

async function gcsRequest(input: {
  method: "GET" | "PUT";
  bucket: string;
  key: string;
  config: AppConfig;
  body?: Uint8Array;
  contentType?: string;
}): Promise<Response> {
  if (input.config.storage.backend !== "gcs") {
    throw new Error("GCS object path requires GCS storage configuration");
  }
  const token = await googleAccessToken();
  const headers: Record<string, string> = {
    Authorization: `Bearer ${token}`,
  };
  const init: RequestInit = {
    method: input.method,
    headers,
  };
  let url: string;
  if (input.method === "PUT") {
    if (input.contentType) {
      headers["content-type"] = input.contentType;
    }
    init.body = toArrayBuffer(input.body ?? new Uint8Array());
    url = `https://storage.googleapis.com/upload/storage/v1/b/${encodeURIComponent(input.bucket)}/o?uploadType=media&name=${encodeURIComponent(input.key)}`;
  } else {
    url = `https://storage.googleapis.com/storage/v1/b/${encodeURIComponent(input.bucket)}/o/${encodeURIComponent(input.key)}?alt=media`;
  }
  const response = await fetch(url, init);
  if (!response.ok) {
    throw new Error(`GCS ${input.method} ${input.bucket}/${input.key} failed with HTTP ${response.status}`);
  }
  return response;
}

async function googleAccessToken(): Promise<string> {
  const response = await fetch(
    "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token",
    { headers: { "Metadata-Flavor": "Google" } },
  );
  if (!response.ok) {
    throw new Error(`failed to fetch Google metadata access token: HTTP ${response.status}`);
  }
  const payload = await response.json() as { access_token?: unknown };
  if (typeof payload.access_token !== "string" || payload.access_token.trim().length === 0) {
    throw new Error("Google metadata access token response missing access_token");
  }
  return payload.access_token.trim();
}

function signedR2Headers(input: {
  method: "GET" | "PUT";
  host: string;
  canonicalPath: string;
  payloadHash: string;
  contentType: string | undefined;
  now: Date;
  accessKeyId: string;
  secretAccessKey: string;
}): Record<string, string> {
  const amzDate = amzTimestamp(input.now);
  const dateStamp = amzDate.slice(0, 8);
  const headers: Record<string, string> = {
    host: input.host,
    "x-amz-content-sha256": input.payloadHash,
    "x-amz-date": amzDate,
  };
  if (input.contentType) {
    headers["content-type"] = input.contentType;
  }
  const signedHeaderNames = Object.keys(headers).sort();
  const canonicalHeaders = signedHeaderNames.map((name) => `${name}:${headers[name]}\n`).join("");
  const credentialScope = `${dateStamp}/auto/s3/aws4_request`;
  const canonicalRequest = [
    input.method,
    input.canonicalPath,
    "",
    canonicalHeaders,
    signedHeaderNames.join(";"),
    input.payloadHash,
  ].join("\n");
  const stringToSign = [
    "AWS4-HMAC-SHA256",
    amzDate,
    credentialScope,
    sha256Hex(canonicalRequest),
  ].join("\n");
  const signature = hmacHex(signingKey(input.secretAccessKey, dateStamp), stringToSign);
  return {
    ...headers,
    Authorization:
      `AWS4-HMAC-SHA256 Credential=${input.accessKeyId}/${credentialScope}, ` +
      `SignedHeaders=${signedHeaderNames.join(";")}, Signature=${signature}`,
  };
}

function signingKey(secretAccessKey: string, dateStamp: string): Uint8Array {
  const kDate = hmacBytes(`AWS4${secretAccessKey}`, dateStamp);
  const kRegion = hmacBytes(kDate, "auto");
  const kService = hmacBytes(kRegion, "s3");
  return hmacBytes(kService, "aws4_request");
}

function sha256Hex(value: Uint8Array | string): string {
  return createHash("sha256").update(value).digest("hex");
}

function hmacBytes(key: Uint8Array | string, value: string): Uint8Array {
  return createHmac("sha256", key).update(value).digest();
}

function hmacHex(key: Uint8Array | string, value: string): string {
  return createHmac("sha256", key).update(value).digest("hex");
}

function amzTimestamp(date: Date): string {
  return date.toISOString().replace(/[:-]|\.\d{3}/g, "");
}

function encodePathSegment(value: string): string {
  return encodeURIComponent(value).replace(/[!'()*]/g, (char) =>
    `%${char.charCodeAt(0).toString(16).toUpperCase()}`
  );
}

function toArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}
