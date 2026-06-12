import { describe, expect, test } from "bun:test";
import { errorResponse, optionalQueryInteger, queryInteger, readJsonObject } from "../src/http";

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

  test("strictly parses bounded integer query parameters", () => {
    expect(queryInteger(
      new URL("https://cloud.example/v1/runs"),
      "limit",
      { defaultValue: 50, min: 1, max: 200 },
    )).toBe(50);
    expect(queryInteger(
      new URL("https://cloud.example/v1/runs?limit=12"),
      "limit",
      { defaultValue: 50, min: 1, max: 200 },
    )).toBe(12);
    expect(optionalQueryInteger(
      new URL("https://cloud.example/v1/runs?after_row_seq=0"),
      "after_row_seq",
      { min: 0 },
    )).toBe(0);
    expect(() => queryInteger(new URL("https://cloud.example/v1/runs?limit=12x"), "limit", { min: 1 }))
      .toThrow("/limit must be an integer");
    expect(() => queryInteger(new URL("https://cloud.example/v1/runs?limit=0"), "limit", { min: 1 }))
      .toThrow("/limit must be >= 1");
    expect(() => queryInteger(new URL("https://cloud.example/v1/runs?limit=201"), "limit", { max: 200 }))
      .toThrow("/limit must be <= 200");
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
