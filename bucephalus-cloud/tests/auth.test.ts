import { describe, expect, test } from "bun:test";
import { OAuthVerifier } from "../src/auth";
import { HttpError } from "../src/http";

describe("OAuth verifier", () => {
  test("accepts the explicit local dev token", async () => {
    const verifier = new OAuthVerifier({
      required: true,
      issuer: null,
      audience: null,
      jwksUrl: null,
      devToken: "local-user-token",
    });

    const auth = await verifier.requireUser(
      new Request("http://localhost/v1/imports", {
        headers: { authorization: "Bearer local-user-token" },
      }),
      "Cloud API",
    );

    expect(auth?.subject).toBe("local-dev");
  });

  test("rejects missing user authentication when required", async () => {
    const verifier = new OAuthVerifier({
      required: true,
      issuer: null,
      audience: null,
      jwksUrl: null,
      devToken: "local-user-token",
    });

    await expect(verifier.requireUser(new Request("http://localhost/v1/imports"), "Cloud API"))
      .rejects
      .toThrow(HttpError);
  });

  test("can be disabled for intentionally unauthenticated local development", async () => {
    const verifier = new OAuthVerifier({
      required: false,
      issuer: null,
      audience: null,
      jwksUrl: null,
      devToken: null,
    });

    await expect(verifier.requireUser(new Request("http://localhost/v1/imports"), "Cloud API"))
      .resolves
      .toBeNull();
  });
});
