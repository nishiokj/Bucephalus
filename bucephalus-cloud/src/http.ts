import { timingSafeEqual } from "node:crypto";

const DEFAULT_MAX_JSON_BODY_BYTES = 1024 * 1024;

export class HttpError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly detail?: Record<string, unknown>,
  ) {
    super(message);
    this.name = "HttpError";
  }
}

export function jsonResponse(value: unknown, init: ResponseInit = {}): Response {
  return new Response(JSON.stringify(value, null, 2), {
    ...init,
    headers: {
      "content-type": "application/json; charset=utf-8",
      ...init.headers,
    },
  });
}

export async function readJsonObject(request: Request): Promise<Record<string, unknown>> {
  const text = await readBoundedRequestText(request, maxJsonBodyBytes());
  const value = parseJson(text);
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_body", "Request body must be a JSON object");
  }
  return value;
}

export function queryInteger(
  url: URL,
  key: string,
  options: { defaultValue?: number; min?: number; max?: number } = {},
): number {
  const raw = url.searchParams.get(key);
  if (raw === null || raw === "") {
    if (options.defaultValue !== undefined) {
      return options.defaultValue;
    }
    throw new HttpError(400, "invalid_query", `/${key} is required`);
  }
  if (!/^[0-9]+$/.test(raw)) {
    throw new HttpError(400, "invalid_query", `/${key} must be an integer`);
  }
  const parsed = Number.parseInt(raw, 10);
  if (!Number.isSafeInteger(parsed)) {
    throw new HttpError(400, "invalid_query", `/${key} must be a safe integer`);
  }
  if (options.min !== undefined && parsed < options.min) {
    throw new HttpError(400, "invalid_query", `/${key} must be >= ${options.min}`);
  }
  if (options.max !== undefined && parsed > options.max) {
    throw new HttpError(400, "invalid_query", `/${key} must be <= ${options.max}`);
  }
  return parsed;
}

export function optionalQueryInteger(
  url: URL,
  key: string,
  options: { min?: number; max?: number } = {},
): number | undefined {
  if (!url.searchParams.has(key)) {
    return undefined;
  }
  return queryInteger(url, key, options);
}

async function readBoundedRequestText(request: Request, maxBytes: number): Promise<string> {
  const contentLength = request.headers.get("content-length");
  if (contentLength !== null) {
    const normalizedContentLength = contentLength.trim();
    const declared = /^[0-9]+$/.test(normalizedContentLength)
      ? Number.parseInt(normalizedContentLength, 10)
      : NaN;
    if (!Number.isSafeInteger(declared) || declared < 0) {
      throw new HttpError(400, "invalid_content_length", "Invalid content-length");
    }
    if (declared > maxBytes) {
      throw new HttpError(413, "request_body_too_large", "Request JSON body exceeds the configured size limit", {
        max_json_body_bytes: maxBytes,
      });
    }
  }
  if (!request.body) {
    return "";
  }
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      const chunk = value instanceof Uint8Array ? value : new Uint8Array(value);
      total += chunk.byteLength;
      if (total > maxBytes) {
        throw new HttpError(413, "request_body_too_large", "Request JSON body exceeds the configured size limit", {
          max_json_body_bytes: maxBytes,
        });
      }
      chunks.push(chunk);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(bytes);
}

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    throw new HttpError(400, "invalid_json", "Request body must be valid JSON");
  }
}

export function errorResponse(error: unknown): Response {
  if (error instanceof HttpError) {
    return jsonResponse(
      {
        code: error.code,
        message: error.message,
        detail: error.detail ?? {},
      },
      { status: error.status },
    );
  }

  const message = exposeInternalErrors()
    ? error instanceof Error ? error.message : "Unknown error"
    : "Internal server error";
  return jsonResponse(
    {
      code: "internal_error",
      message,
    },
    { status: 500 },
  );
}

function maxJsonBodyBytes(): number {
  const raw = process.env.BUCEPHALUS_CLOUD_MAX_JSON_BODY_BYTES;
  if (!raw) {
    return DEFAULT_MAX_JSON_BODY_BYTES;
  }
  const parsed = Number.parseInt(raw, 10);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : DEFAULT_MAX_JSON_BODY_BYTES;
}

function exposeInternalErrors(): boolean {
  const value = process.env.BUCEPHALUS_CLOUD_EXPOSE_INTERNAL_ERRORS;
  return value !== undefined && ["1", "true", "yes", "on"].includes(value.trim().toLowerCase());
}

export function requireString(value: unknown, pointer: string): string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new HttpError(400, "invalid_request", `${pointer} must be a non-empty string`);
  }
  return value;
}

export function optionalString(value: unknown, pointer: string): string | null {
  if (value === undefined || value === null) {
    return null;
  }
  if (typeof value !== "string") {
    throw new HttpError(400, "invalid_request", `${pointer} must be a string`);
  }
  return value;
}

export function requireRecord(value: unknown, pointer: string): Record<string, unknown> {
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_request", `${pointer} must be an object`);
  }
  return value;
}

export function requireBearerToken(request: Request, expectedToken: string, scope: string): void {
  requireStaticToken(request, expectedToken, {
    scope,
    credentialName: "worker token",
    headerNames: ["x-bucephalus-worker-token"],
  });
}

export function requireStaticToken(
  request: Request,
  expectedToken: string,
  options: { scope: string; credentialName: string; headerNames?: string[] },
): void {
  const authorization = request.headers.get("authorization");
  const bearer = authorization?.startsWith("Bearer ") ? authorization.slice("Bearer ".length) : null;
  const headerToken = (options.headerNames ?? [])
    .map((name) => request.headers.get(name))
    .find((token): token is string => typeof token === "string" && token.length > 0);
  const providedToken = bearer ?? headerToken;
  if (!providedToken || !secureEqual(providedToken, expectedToken)) {
    throw new HttpError(401, "unauthorized", `${options.scope} requires a valid ${options.credentialName}`);
  }
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function secureEqual(left: string, right: string): boolean {
  const leftBytes = Buffer.from(left);
  const rightBytes = Buffer.from(right);
  return leftBytes.byteLength === rightBytes.byteLength && timingSafeEqual(leftBytes, rightBytes);
}
