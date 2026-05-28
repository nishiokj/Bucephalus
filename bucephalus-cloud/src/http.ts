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
  const value = await request.json().catch(() => {
    throw new HttpError(400, "invalid_json", "Request body must be valid JSON");
  });
  if (!isRecord(value)) {
    throw new HttpError(400, "invalid_body", "Request body must be a JSON object");
  }
  return value;
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

  const message = error instanceof Error ? error.message : "Unknown error";
  return jsonResponse(
    {
      code: "internal_error",
      message,
    },
    { status: 500 },
  );
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

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

