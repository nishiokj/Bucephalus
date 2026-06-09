import { describe, expect, test } from "bun:test";
import { publicBoundaryJsonObject, publicBoundaryText } from "../src/publicBoundary";

describe("public boundary redaction", () => {
  test("redacts cross-platform personal paths in text", () => {
    const redacted = publicBoundaryText([
      "posix=/home/alice/project/log.txt",
      "mac=/Volumes/Backup Drive/secrets.txt",
      String.raw`win=C:\Users\Alice\AppData\Local\Temp\buc.log`,
      String.raw`env=%USERPROFILE%\Documents\bench\run.json`,
      "home=~/Library/Application Support/bucephalus/state.json",
      "wsl=/mnt/c/Users/Alice/AppData/Local/Temp/buc.log",
    ].join(" "));

    expect(redacted).toContain("posix=[redacted-local-path]");
    expect(redacted).toContain("mac=[redacted-local-path]");
    expect(redacted).toContain("win=[redacted-local-path]");
    expect(redacted).toContain("env=[redacted-local-path]");
    expect(redacted).toContain("home=[redacted-local-path]");
    expect(redacted).toContain("wsl=[redacted-local-path]");
    expect(redacted).not.toContain("/home/alice");
    expect(redacted).not.toContain("/Volumes/Backup");
    expect(redacted).not.toContain("Drive/secrets");
    expect(redacted).not.toContain(String.raw`C:\Users\Alice`);
    expect(redacted).not.toContain("%USERPROFILE%");
    expect(redacted).not.toContain("~/Library");
    expect(redacted).not.toContain("Application Support");
    expect(redacted).not.toContain("/mnt/c/Users/Alice");
  });

  test("redacts cross-platform paths recursively in public JSON", () => {
    const body = publicBoundaryJsonObject({
      workspace: String.raw`C:\Users\Alice\workspace`,
      nested: {
        cache: "%LOCALAPPDATA%\\Bucephalus\\cache",
        note: "safe",
      },
    });

    expect(body).toEqual({
      workspace: "[redacted-local-path]",
      nested: {
        cache: "[redacted-local-path]",
        note: "safe",
      },
    });
  });

  test("redacts provider secret refs in text and public JSON", () => {
    const text = publicBoundaryText([
      "ref=gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
      "aws=aws-secrets-manager://us-east-1/secret/prod/openai",
    ].join(" "));

    expect(text).toContain("[redacted-secret]");
    expect(text).not.toContain("gcp-secret-manager://");
    expect(text).not.toContain("aws-secrets-manager://");

    const body = publicBoundaryJsonObject({
      secret_ref: "gcp-secret-manager://projects/acme/secrets/openai/versions/latest",
      nested: {
        message: "using aws-secrets-manager://us-east-1/secret/prod/openai",
      },
    });

    expect(body?.secret_ref).toBe("[redacted]");
    expect(body?.nested).toEqual({
      message: "using [redacted-secret]",
    });
  });
});
