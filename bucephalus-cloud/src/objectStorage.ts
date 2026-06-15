import { createHash, createHmac } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { loadConfig, type AppConfig } from "./config";

const EMPTY_SHA256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

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

  const key = objectKey(config.storage.prefix, "uploads", safeObjectSegment(uploadId), "content.blob");
  if (config.storage.backend === "gcs") {
    await gcsRequest({
      method: "PUT",
      bucket: config.storage.bucket,
      key,
      body: bytes,
      contentType: mediaType,
    });
    return gcsUri(config.storage.bucket, key);
  }

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

export async function putRuntimeObject(
  input: {
    cloudRunId: string;
    attemptId: string;
    coreRunId: string;
    trialId: string;
    trialAttempt: number;
    role: string;
    bytes: Uint8Array;
    mediaType: string;
  },
  config: AppConfig = loadConfig(),
): Promise<string> {
  const parts = [
    "runtime-objects",
    safeObjectSegment(input.cloudRunId),
    safeObjectSegment(input.attemptId),
    safeObjectSegment(input.coreRunId),
    safeObjectSegment(input.trialId),
    safeObjectSegment(String(input.trialAttempt)),
    safeObjectSegment(input.role),
    "content.blob",
  ];
  if (config.storage.backend === "filesystem") {
    const storagePath = join(config.dataDir, ...parts);
    await mkdir(join(config.dataDir, ...parts.slice(0, -1)), { recursive: true });
    await writeFile(storagePath, input.bytes);
    return storagePath;
  }

  const key = objectKey(config.storage.prefix, ...parts);
  if (config.storage.backend === "gcs") {
    await gcsRequest({
      method: "PUT",
      bucket: config.storage.bucket,
      key,
      body: input.bytes,
      contentType: input.mediaType,
    });
    return gcsUri(config.storage.bucket, key);
  }

  await r2Request({
    method: "PUT",
    bucket: config.storage.bucket,
    key,
    body: input.bytes,
    contentType: input.mediaType,
    config,
  });
  return r2Uri(config.storage.bucket, key);
}

export async function readStoredObject(storagePath: string, config: AppConfig = loadConfig()): Promise<Uint8Array> {
  const gcsObject = parseGcsUri(storagePath);
  if (gcsObject) {
    if (config.storage.backend === "gcs" && gcsObject.bucket !== config.storage.bucket) {
      throw new Error(`GCS object bucket ${gcsObject.bucket} does not match configured bucket ${config.storage.bucket}`);
    }
    const response = await gcsRequest({
      method: "GET",
      bucket: gcsObject.bucket,
      key: gcsObject.key,
    });
    return new Uint8Array(await response.arrayBuffer());
  }

  const r2Object = parseR2Uri(storagePath);
  if (!r2Object) {
    return new Uint8Array(await readFile(storagePath));
  }
  if (config.storage.backend === "r2" && r2Object.bucket !== config.storage.bucket) {
    throw new Error(`R2 object bucket ${r2Object.bucket} does not match configured bucket ${config.storage.bucket}`);
  }
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
  if (!parseR2Uri(storagePath) && !parseGcsUri(storagePath)) {
    return storagePath;
  }
  await mkdir(workDir, { recursive: true });
  const localPath = join(workDir, filename);
  await writeFile(localPath, await readStoredObject(storagePath, config));
  return localPath;
}

function parseGcsUri(storagePath: string): { bucket: string; key: string } | null {
  if (!storagePath.startsWith("gcs://")) {
    return null;
  }
  const url = new URL(storagePath);
  return {
    bucket: url.hostname,
    key: decodeURIComponent(url.pathname.replace(/^\/+/, "")),
  };
}

function parseR2Uri(storagePath: string): { bucket: string; key: string } | null {
  if (!storagePath.startsWith("r2://")) {
    return null;
  }
  const url = new URL(storagePath);
  return {
    bucket: url.hostname,
    key: decodeURIComponent(url.pathname.replace(/^\/+/, "")),
  };
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
  body?: Uint8Array;
  contentType?: string;
}): Promise<Response> {
  const token = await gcsAccessToken();
  const headers: Record<string, string> = {
    authorization: `Bearer ${token}`,
  };
  let url: string;
  const init: RequestInit = {
    method: input.method === "PUT" ? "POST" : "GET",
    headers,
  };
  if (input.method === "PUT") {
    if (input.contentType) {
      headers["content-type"] = input.contentType;
    }
    init.body = toArrayBuffer(input.body ?? new Uint8Array());
    const params = new URLSearchParams({
      uploadType: "media",
      name: input.key,
    });
    url = `https://storage.googleapis.com/upload/storage/v1/b/${encodeURIComponent(input.bucket)}/o?${params}`;
  } else {
    const params = new URLSearchParams({ alt: "media" });
    url = `https://storage.googleapis.com/storage/v1/b/${encodeURIComponent(input.bucket)}/o/${encodeURIComponent(input.key)}?${params}`;
  }
  const response = await fetch(url, init);
  if (!response.ok) {
    throw new Error(`GCS ${input.method} ${input.bucket}/${input.key} failed with HTTP ${response.status}`);
  }
  return response;
}

async function gcsAccessToken(): Promise<string> {
  const response = await fetch(
    gcpMetadataUrl("/computeMetadata/v1/instance/service-accounts/default/token"),
    {
      headers: {
        "Metadata-Flavor": "Google",
      },
    },
  );
  if (!response.ok) {
    throw new Error(`GCS metadata token request failed with HTTP ${response.status}`);
  }
  const body = await response.json();
  if (!body || typeof body.access_token !== "string" || body.access_token.length === 0) {
    throw new Error("GCS metadata token response did not include an access_token");
  }
  return body.access_token;
}

function gcpMetadataUrl(path: string): string {
  const metadataProtocol = "http";
  return `${metadataProtocol}://metadata.google.internal${path}`;
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
