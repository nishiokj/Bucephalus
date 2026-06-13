import { authOwnerKey, bearerToken, isTokenAuth, type AuthContext } from "../auth";
import type { AuthConfig } from "../config";
import { HttpError, jsonResponse, optionalString, readJsonObject } from "../http";
import { TOKEN_SECRET_PREFIX, type ApiTokenRecord, type ApiTokenRepository } from "../tokens/repository";

// Sliding window: any authenticated request inside 30 days keeps the session
// alive; 30 days of silence ends it.
const SESSION_TTL_SECONDS = 30 * 24 * 60 * 60;
const MAX_LABEL_LENGTH = 120;

export interface AuthRouteDependencies {
  exchangeOAuthCode?: (input: OAuthCodeExchangeInput) => Promise<Record<string, unknown>>;
  verifyOAuthToken?: (token: string) => Promise<AuthContext>;
}

export interface OAuthCodeExchangeInput {
  authConfig: AuthConfig;
  code: string;
  redirectUri: string;
  codeVerifier: string;
}

export async function handleAuthRoute(
  request: Request,
  url: URL,
  tokens: ApiTokenRepository,
  auth?: AuthContext | null,
  authConfig?: AuthConfig,
  dependencies: AuthRouteDependencies = {},
): Promise<Response | null> {
  if (!url.pathname.startsWith("/v1/auth/")) {
    return null;
  }
  if (request.method === "GET" && url.pathname === "/v1/auth/config") {
    return jsonResponse(publicAuthConfig(authConfig));
  }

  if (request.method === "POST" && url.pathname === "/v1/auth/oauth/exchange") {
    if (!authConfig?.cliClientId || !authConfig.cliClientSecret) {
      throw new HttpError(
        503,
        "oauth_exchange_not_configured",
        "Hosted OAuth code exchange is not configured; the Cloud API must hold the CLI OAuth client secret",
      );
    }
    if (!dependencies.verifyOAuthToken) {
      throw new HttpError(503, "oauth_verifier_not_configured", "Hosted OAuth verifier is not configured");
    }
    const body = await readJsonObject(request);
    const code = requiredBodyString(body, "code");
    const redirectUri = requiredBodyString(body, "redirect_uri");
    const codeVerifier = requiredBodyString(body, "code_verifier");
    const clientId = requiredBodyString(body, "client_id");
    if (clientId !== authConfig.cliClientId) {
      throw new HttpError(400, "invalid_client", "OAuth code exchange client_id does not match hosted auth config");
    }
    if (!/^http:\/\/127\.0\.0\.1:[0-9]{2,5}\/callback$/.test(redirectUri)) {
      throw new HttpError(400, "invalid_redirect_uri", "OAuth code exchange redirect_uri must be the buc loopback callback");
    }
    const token = await (dependencies.exchangeOAuthCode ?? exchangeOAuthCode)({
      authConfig,
      code,
      redirectUri,
      codeVerifier,
    });
    const idToken = requiredTokenString(token, "id_token");
    const oauth = await dependencies.verifyOAuthToken(idToken);
    const { record, secret } = await tokens.createToken({
      kind: "session",
      issuer: oauth.issuer,
      subject: oauth.subject,
      ttlSeconds: SESSION_TTL_SECONDS,
    });
    return jsonResponse({
      schema_version: "bucephalus_cloud_login_session_v1",
      ...tokenToWire(record),
      token: secret,
      token_type: "Bearer",
      access_token: secret,
      expires_in: SESSION_TTL_SECONDS,
    }, { status: 201 });
  }

  if (!auth) {
    throw new HttpError(401, "unauthorized", "Auth management requires an authenticated user");
  }

  if (request.method === "POST" && url.pathname === "/v1/auth/sessions") {
    // Exchange is the trust boundary between the identity provider and the
    // platform: only a fresh OAuth credential proves the user is present.
    // Letting an existing session or API key mint sessions would make any
    // leaked token self-renewing forever.
    if (isTokenAuth(auth)) {
      throw new HttpError(403, "oauth_credential_required", "Session creation requires an identity-provider credential, not a Bucephalus token");
    }
    const { record, secret } = await tokens.createToken({
      kind: "session",
      issuer: auth.issuer,
      subject: auth.subject,
      ttlSeconds: SESSION_TTL_SECONDS,
    });
    return jsonResponse({ ...tokenToWire(record), token: secret }, { status: 201 });
  }

  if (request.method === "DELETE" && url.pathname === "/v1/auth/sessions/current") {
    const secret = bearerToken(request);
    if (!secret?.startsWith(TOKEN_SECRET_PREFIX)) {
      throw new HttpError(400, "not_a_session", "Sign-out revokes the bearer session token; the request used a different credential");
    }
    const record = await tokens.revokeBySecret(secret);
    return jsonResponse({ revoked: record !== null });
  }

  if (request.method === "POST" && url.pathname === "/v1/auth/api-keys") {
    const body = await readJsonObject(request);
    const label = optionalString(body.label, "/label")?.trim() || null;
    if (label && label.length > MAX_LABEL_LENGTH) {
      throw new HttpError(400, "invalid_request", `/label must be at most ${MAX_LABEL_LENGTH} characters`);
    }
    const { record, secret } = await tokens.createToken({
      kind: "api_key",
      issuer: auth.issuer,
      subject: auth.subject,
      label,
    });
    return jsonResponse({ ...tokenToWire(record), token: secret }, { status: 201 });
  }

  if (request.method === "GET" && url.pathname === "/v1/auth/tokens") {
    const records = await tokens.listTokens(requireOwnerKey(auth));
    return jsonResponse({ tokens: records.map(tokenToWire) });
  }

  if (request.method === "DELETE" && url.pathname.startsWith("/v1/auth/tokens/")) {
    const tokenId = decodeURIComponent(url.pathname.slice("/v1/auth/tokens/".length));
    const record = await tokens.revokeToken(requireOwnerKey(auth), tokenId);
    if (!record) {
      throw new HttpError(404, "token_not_found", "Token not found");
    }
    return jsonResponse({ ...tokenToWire(record), revoked: true });
  }

  return null;
}

function publicAuthConfig(config?: AuthConfig) {
  const audience = config?.cliClientId ?? config?.audiences?.find((value) => value.trim().length > 0) ?? null;
  const value: Record<string, unknown> = {
    schema_version: "bucephalus_cloud_auth_config_v1",
    issuer: config?.issuer ?? null,
    client_id: audience,
    audience,
    scope: config?.cliScope ?? "openid email",
  };
  if (config?.cliClientId && config.cliClientSecret) {
    value.code_exchange_path = "/v1/auth/oauth/exchange";
  }
  return value;
}

function requiredBodyString(body: Record<string, unknown>, key: string): string {
  const value = body[key];
  if (typeof value !== "string" || value.trim() === "") {
    throw new HttpError(400, "invalid_request", `/${key} must be a non-empty string`);
  }
  return value;
}

function requiredTokenString(body: Record<string, unknown>, key: string): string {
  const value = body[key];
  if (typeof value !== "string" || value.trim() === "") {
    throw new HttpError(502, "oauth_exchange_failed", `OAuth token response did not include ${key}`);
  }
  return value;
}

async function exchangeOAuthCode(input: OAuthCodeExchangeInput): Promise<Record<string, unknown>> {
  const tokenEndpoint = await oauthTokenEndpoint(input.authConfig);
  const response = await fetch(tokenEndpoint, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      code: input.code,
      redirect_uri: input.redirectUri,
      client_id: input.authConfig.cliClientId ?? "",
      client_secret: input.authConfig.cliClientSecret ?? "",
      code_verifier: input.codeVerifier,
    }),
  });
  const text = await response.text();
  let body: unknown;
  try {
    body = text ? JSON.parse(text) : {};
  } catch {
    body = {};
  }
  if (!response.ok) {
    throw new HttpError(response.status, "oauth_exchange_failed", "OAuth authorization code exchange failed", {
      provider_error: providerError(body),
    });
  }
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    throw new HttpError(502, "oauth_exchange_failed", "OAuth token endpoint returned an invalid response");
  }
  return body as Record<string, unknown>;
}

async function oauthTokenEndpoint(config: AuthConfig): Promise<string> {
  const issuer = config.issuer?.replace(/\/+$/, "");
  if (!issuer) {
    throw new HttpError(503, "oauth_exchange_not_configured", "OAuth issuer is not configured");
  }
  const response = await fetch(`${issuer}/.well-known/openid-configuration`);
  if (!response.ok) {
    throw new HttpError(503, "oauth_metadata_unavailable", "OAuth metadata endpoint is unavailable");
  }
  const body = await response.json();
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    throw new HttpError(503, "oauth_metadata_invalid", "OAuth metadata endpoint returned an invalid document");
  }
  const tokenEndpoint = (body as Record<string, unknown>).token_endpoint;
  if (typeof tokenEndpoint !== "string" || !tokenEndpoint.startsWith("https://")) {
    throw new HttpError(503, "oauth_metadata_invalid", "OAuth metadata did not include an https token_endpoint");
  }
  return tokenEndpoint;
}

function providerError(body: unknown): Record<string, unknown> | null {
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    return null;
  }
  const record = body as Record<string, unknown>;
  return {
    error: typeof record.error === "string" ? record.error : undefined,
    error_description: typeof record.error_description === "string" ? record.error_description : undefined,
  };
}

function requireOwnerKey(auth: AuthContext): string {
  const ownerKey = authOwnerKey(auth);
  if (!ownerKey) {
    throw new HttpError(401, "unauthorized", "Auth management requires an authenticated user");
  }
  return ownerKey;
}

function tokenToWire(record: ApiTokenRecord) {
  return {
    token_id: record.token_id,
    token_prefix: record.token_prefix,
    kind: record.kind,
    label: record.label,
    expires_at: record.expires_at,
    last_used_at: record.last_used_at,
    created_at: record.created_at,
  };
}
