export interface Env {
  ASSETS: Fetcher;
  BUCEPHALUS_API_BASE?: string;
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/buc-config.js") {
      return configResponse(env);
    }
    return env.ASSETS.fetch(request);
  },
};

function configResponse(env: Env): Response {
  const config: { apiBase?: string } = {};
  const apiBase = env.BUCEPHALUS_API_BASE?.trim();
  if (apiBase) {
    config.apiBase = apiBase;
  }
  return new Response(`window.BUCEPHALUS_WEB_CONFIG = ${JSON.stringify(config)};\n`, {
    headers: {
      "cache-control": "no-store",
      "content-type": "application/javascript; charset=utf-8",
    },
  });
}
