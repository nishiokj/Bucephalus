import { timingSafeEqual } from "node:crypto";
import { publicBoundaryText, publicBoundaryValue } from "./publicBoundary";

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

export async function readOptionalJsonObject(request: Request): Promise<Record<string, unknown>> {
  const text = await readBoundedRequestText(request, maxJsonBodyBytes());
  if (text.trim().length === 0) {
    return {};
  }
  const value = parseJson(text);
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_body", "Request body must be a JSON object");
  }
  return value;
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
        message: publicBoundaryText(error.message),
        detail: publicBoundaryValue(error.detail ?? {}),
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
      message: publicBoundaryText(message),
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

interface QueryIntegerBounds {
  min: number;
  max: number;
}

export function queryIntegerParam(
  url: URL,
  key: string,
  bounds: QueryIntegerBounds & { defaultValue: number },
): number;
export function queryIntegerParam(url: URL, key: string, bounds: QueryIntegerBounds): number | undefined;
export function queryIntegerParam(
  url: URL,
  key: string,
  bounds: QueryIntegerBounds & { defaultValue?: number },
): number | undefined {
  const raw = url.searchParams.get(key);
  if (raw === null) {
    return bounds.defaultValue;
  }
  const normalized = raw.trim();
  const rangeDescription = bounds.min === 0
    ? `an integer from 0 to ${bounds.max}`
    : `an integer from ${bounds.min} to ${bounds.max}`;
  if (!/^[0-9]+$/.test(normalized)) {
    throw new HttpError(400, "invalid_query_param", `${key} must be ${rangeDescription}`, {
      param: key,
      min: bounds.min,
      max: bounds.max,
    });
  }
  const parsed = Number.parseInt(normalized, 10);
  if (!Number.isSafeInteger(parsed) || parsed < bounds.min || parsed > bounds.max) {
    throw new HttpError(400, "invalid_query_param", `${key} must be ${rangeDescription}`, {
      param: key,
      min: bounds.min,
      max: bounds.max,
    });
  }
  return parsed;
}

export function decodePathParam(value: string, pointer: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    throw new HttpError(400, "invalid_path_param", `${pointer} must be valid percent-encoded UTF-8`);
  }
}

export function requireRecord(value: unknown, pointer: string): Record<string, unknown> {
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_request", `${pointer} must be an object`);
  }
  return value;
}

export function requireBearerToken(request: Request, expectedToken: string, scope: string): void {
  const authorization = request.headers.get("authorization");
  const bearer = authorization?.startsWith("Bearer ") ? authorization.slice("Bearer ".length) : null;
  const headerToken = request.headers.get("x-bucephalus-worker-token");
  const providedToken = bearer ?? headerToken;
  if (!providedToken || !secureEqual(providedToken, expectedToken)) {
    throw new HttpError(401, "unauthorized", `${scope} requires a valid worker token`);
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
