import { authOwnerKey, bearerToken, isTokenAuth, type AuthContext } from "../auth";
import { HttpError, jsonResponse, optionalString, readJsonObject } from "../http";
import { TOKEN_SECRET_PREFIX, type ApiTokenRecord, type ApiTokenRepository } from "../tokens/repository";

// Sliding window: any authenticated request inside 30 days keeps the session
// alive; 30 days of silence ends it.
const SESSION_TTL_SECONDS = 30 * 24 * 60 * 60;
const MAX_LABEL_LENGTH = 120;

export async function handleAuthRoute(
  request: Request,
  url: URL,
  tokens: ApiTokenRepository,
  auth?: AuthContext | null,
): Promise<Response | null> {
  if (!url.pathname.startsWith("/v1/auth/")) {
    return null;
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
