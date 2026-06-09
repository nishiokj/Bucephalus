import { describe, expect, test } from "bun:test";
import { decodePathParam, errorResponse, HttpError, queryIntegerParam, readJsonObject, readOptionalJsonObject } from "../src/http";

describe("HTTP helpers", () => {
  test("rejects JSON requests whose declared content-length exceeds the configured cap", async () => {
    const previous = process.env.BUCEPHALUS_CLOUD_MAX_JSON_BODY_BYTES;
    process.env.BUCEPHALUS_CLOUD_MAX_JSON_BODY_BYTES = "3";
    try {
      await expect(readJsonObject(new Request("https://cloud.example/v1/runs", {
        method: "POST",
        headers: {
          "content-length": "4",
          "content-type": "application/json",
        },
        body: "{}",
      }))).rejects.toThrow("Request JSON body exceeds");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_MAX_JSON_BODY_BYTES", previous);
    }
  });

  test("rejects malformed content-length headers", async () => {
    await expect(readJsonObject(new Request("https://cloud.example/v1/runs", {
      method: "POST",
      headers: {
        "content-length": "4junk",
        "content-type": "application/json",
      },
      body: "{}",
    }))).rejects.toThrow("Invalid content-length");
  });

  test("optional JSON requests allow empty bodies but reject malformed bodies", async () => {
    expect(await readOptionalJsonObject(new Request("https://cloud.example/v1/runner-instances/expire-stale", {
      method: "POST",
    }))).toEqual({});

    await expect(readOptionalJsonObject(new Request("https://cloud.example/v1/runner-instances/expire-stale", {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: "{",
    }))).rejects.toThrow("Request body must be valid JSON");
  });

  test("redacts unexpected internal errors by default", async () => {
    const previous = process.env.BUCEPHALUS_CLOUD_EXPOSE_INTERNAL_ERRORS;
    delete process.env.BUCEPHALUS_CLOUD_EXPOSE_INTERNAL_ERRORS;
    try {
      const response = errorResponse(new Error("database password leaked from /tmp/secret.log"));
      const body = await response.json();

      expect(response.status).toBe(500);
      expect(body).toEqual({
        code: "internal_error",
        message: "Internal server error",
      });
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_EXPOSE_INTERNAL_ERRORS", previous);
    }
  });

  test("redacts HttpError messages and details before responding", async () => {
    const response = errorResponse(new HttpError(
      400,
      "invalid_request",
      "failed to read /Users/alice/private/project token=raw-http-token file:///private/tmp/log",
      {
        path: "/Users/alice/private/project",
        mirror: "https://alice:secret@example.com/archive.tgz?token=raw-query#debug",
        nested: {
          api_key: "sk-abcdefghijklmnopqrstuvwxyz",
          note: "ok",
        },
      },
    ));
    const body = await response.json();
    const text = JSON.stringify(body);

    expect(response.status).toBe(400);
    expect(body.code).toBe("invalid_request");
    expect(body.message).toBe(
      "failed to read [redacted-local-path] token=[redacted-secret] file://[redacted-local-path]",
    );
    expect(body.detail).toEqual({
      path: "[redacted-local-path]",
      mirror: "https://example.com/archive.tgz [redacted URL credentials/query]",
      nested: {
        api_key: "[redacted]",
        note: "ok",
      },
    });
    expect(text).not.toContain("/Users/alice");
    expect(text).not.toContain("raw-http-token");
    expect(text).not.toContain("raw-query");
    expect(text).not.toContain("sk-abcdefghijklmnopqrstuvwxyz");
  });

  test("query integer params reject invalid values without echoing the raw query", () => {
    const url = new URL("https://cloud.example/v1/runs?limit=7");
    expect(queryIntegerParam(url, "limit", { defaultValue: 50, min: 1, max: 10 })).toBe(7);
    expect(queryIntegerParam(url, "after_row_seq", { min: 0, max: 10 })).toBeUndefined();

    let caught: unknown;
    try {
      queryIntegerParam(
        new URL("https://cloud.example/v1/runs?limit=token=raw-query-token"),
        "limit",
        { defaultValue: 50, min: 1, max: 10 },
      );
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_query_param");
    expect(error.message).toBe("limit must be an integer from 1 to 10");
    expect(error.detail).toEqual({ param: "limit", min: 1, max: 10 });
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-query-token");
  });

  test("path params reject malformed percent encoding without echoing the raw path", () => {
    expect(decodePathParam("run-1%2Fnested", "/run_id")).toBe("run-1/nested");

    let caught: unknown;
    try {
      decodePathParam("%E0%A4%A-token=raw-path-token", "/run_id");
    } catch (error) {
      caught = error;
    }

    expect(caught).toBeInstanceOf(HttpError);
    const error = caught as HttpError;
    expect(error.status).toBe(400);
    expect(error.code).toBe("invalid_path_param");
    expect(error.message).toBe("/run_id must be valid percent-encoded UTF-8");
    expect(JSON.stringify({ message: error.message, detail: error.detail })).not.toContain("raw-path-token");
  });

  test("can expose unexpected internal errors when explicitly enabled for debugging", async () => {
    const previous = process.env.BUCEPHALUS_CLOUD_EXPOSE_INTERNAL_ERRORS;
    process.env.BUCEPHALUS_CLOUD_EXPOSE_INTERNAL_ERRORS = "true";
    try {
      const response = errorResponse(new Error("debug failure"));
      const body = await response.json();

      expect(response.status).toBe(500);
      expect(body.message).toBe("debug failure");
    } finally {
      restoreEnv("BUCEPHALUS_CLOUD_EXPOSE_INTERNAL_ERRORS", previous);
    }
  });
});

function restoreEnv(name: string, value: string | undefined): void {
  if (value === undefined) {
    delete process.env[name];
  } else {
    process.env[name] = value;
  }
}
