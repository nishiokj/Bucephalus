type Json = Record<string, unknown>;

declare global {
  interface Window {
    BUCEPHALUS_WEB_CONFIG?: {
      apiBase?: string;
      userToken?: string;
      workerToken?: string;
      runnerPoolId?: string;
    };
  }
}

interface AppState {
  activeView: "overview" | "packages" | "authoring" | "resource" | "runs" | "runners";
  authoringMode: "experiment" | "registry";
  apiBase: string;
  userToken: string;
  workerToken: string;
  runnerPoolId: string;
  loading: boolean;
  message: string;
  error: string;
  health: Json | null;
  packages: Json[];
  imports: Json[];
  runs: Json[];
  pools: Json[];
  instances: Json[];
  provisions: Json[];
  registryHits: Json[];
  registryReview: Json | null;
  selectedResource: Json | null;
  selectedResourceAliases: Json[];
  draftPreview: Json | null;
  draftValidation: Json | null;
  draftYaml: string;
  draftForm: Record<string, string>;
  draftResourceReviews: Json[];
  draftDirty: boolean;
}

const state: AppState = {
  activeView: "overview",
  authoringMode: "experiment",
  apiBase: localStorage.getItem("buc.apiBase") || window.BUCEPHALUS_WEB_CONFIG?.apiBase || "http://localhost:8099",
  userToken: localStorage.getItem("buc.userToken") || window.BUCEPHALUS_WEB_CONFIG?.userToken || "",
  workerToken: localStorage.getItem("buc.workerToken") || window.BUCEPHALUS_WEB_CONFIG?.workerToken || "",
  runnerPoolId: localStorage.getItem("buc.runnerPoolId") || window.BUCEPHALUS_WEB_CONFIG?.runnerPoolId || "",
  loading: false,
  message: "",
  error: "",
  health: null,
  packages: [],
  imports: [],
  runs: [],
  pools: [],
  instances: [],
  provisions: [],
  registryHits: [],
  registryReview: null,
  selectedResource: null,
  selectedResourceAliases: [],
  draftPreview: null,
  draftValidation: null,
  draftYaml: "",
  draftForm: defaultDraftForm(),
  draftResourceReviews: [],
  draftDirty: false,
};

const app = document.querySelector<HTMLDivElement>("#app");
if (!app) {
  throw new Error("missing #app");
}

render();
void refreshAll();

function render(): void {
  app.innerHTML = `
    <div class="shell">
      <aside class="sidebar">
        <div class="brand">
          <h1>Bucephalus Cloud</h1>
          <span>Experiment control plane</span>
        </div>
        <nav class="nav">
          ${navButton("overview", "Overview", countActiveRuns())}
          ${navButton("packages", "Builds", state.packages.length)}
          ${navButton("authoring", "Design", state.registryHits.length)}
          ${navButton("runs", "Run", state.runs.length)}
          ${navButton("runners", "Execution", liveRunnerCount())}
        </nav>
      </aside>
      <main class="main">
        <div class="topbar">
          <div>
            <h2>${titleForView(state.activeView)}</h2>
            <p>${subtitleForView(state.activeView)}</p>
          </div>
          <form class="settings" data-action="settings">
            <label class="field">
              <span class="label">API</span>
              <input name="apiBase" value="${escapeAttr(state.apiBase)}" />
            </label>
            <label class="field">
              <span class="label">User token</span>
              <input name="userToken" type="password" value="${escapeAttr(state.userToken)}" />
            </label>
            <label class="field">
              <span class="label">Worker token</span>
              <input name="workerToken" type="password" value="${escapeAttr(state.workerToken)}" />
            </label>
            <label class="field compact">
              <span class="label">Execution service</span>
              <input name="runnerPoolId" value="${escapeAttr(state.runnerPoolId)}" />
            </label>
            <button type="submit">Save</button>
          </form>
        </div>
        ${state.message ? `<div class="section message">${escapeHtml(state.message)}</div>` : ""}
        ${state.error ? `<div class="section message error">${escapeHtml(state.error)}</div>` : ""}
        ${viewMarkup()}
      </main>
    </div>
  `;
  bindEvents();
}

function navButton(view: AppState["activeView"], label: string, count: number): string {
  return `<button type="button" data-view="${view}" class="${state.activeView === view ? "active" : ""}">
    <span>${label}</span><span class="pill">${count}</span>
  </button>`;
}

function viewMarkup(): string {
  switch (state.activeView) {
    case "packages":
      return packagesView();
    case "authoring":
      return authoringView();
    case "resource":
      return resourceView();
    case "runs":
      return runsView();
    case "runners":
      return runnersView();
    default:
      return overviewView();
  }
}

function overviewView(): string {
  return `
    <section class="section grid">
      ${metric("API", state.health ? "Ready" : "Unknown", state.health ? "ok" : "warn")}
      ${metric("Queued Experiments", String(countQueuedRuns()), countQueuedRuns() > 0 ? "warn" : "ok")}
      ${metric("In Progress", String(countStatus(state.runs, "running")), countStatus(state.runs, "running") > 0 ? "info" : "ok")}
      ${metric("Ready Workers", String(liveRunnerCount()), liveRunnerCount() > 0 ? "ok" : "warn")}
      ${metric("Starting Workers", String(state.provisions.filter((p) => ["requested", "provisioning"].includes(String(p.status))).length), "violet")}
      ${metric("Builds", String(state.packages.length), state.packages.length > 0 ? "ok" : "warn")}
    </section>
    <section class="section grid">
      <div class="span-6 panel">
        <div class="section-head"><h3>Recent Experiments</h3><button type="button" data-view="runs">Open</button></div>
        ${runsTable(state.runs.slice(0, 6))}
      </div>
      <div class="span-6 panel">
        <div class="section-head"><h3>Execution Service</h3><button type="button" data-view="runners">Open</button></div>
        ${managedServiceTable()}
      </div>
    </section>
  `;
}

function packagesView(): string {
  return `
    <section class="section panel">
      <div class="section-head">
        <h3>Upload Experiment Build</h3>
        <button type="button" data-action="refresh" ${state.loading ? "disabled" : ""}>Refresh</button>
      </div>
      <form class="form-grid" data-action="import-package">
        <label class="field span-3">
          <span class="label">Build archive</span>
          <input type="file" name="file" />
        </label>
        <label class="field span-2">
          <span class="label">Label</span>
          <input name="label" placeholder="smoke, benchmark-v2" />
        </label>
        <div class="field">
          <span class="label">&nbsp;</span>
          <button class="primary" type="submit" ${state.loading ? "disabled" : ""}>Upload Build</button>
        </div>
      </form>
    </section>
    <section class="section panel">
      <div class="section-head"><h3>Experiment Builds</h3></div>
      ${packageCards(state.packages)}
    </section>
    <section class="section panel">
      <div class="section-head"><h3>Upload History</h3></div>
      ${importsTable(state.imports)}
    </section>
  `;
}

function authoringView(): string {
  return `
    <section class="section panel">
      <div class="segmented">
        <button type="button" data-authoring-mode="experiment" class="${state.authoringMode === "experiment" ? "active" : ""}">Design Experiment</button>
        <button type="button" data-authoring-mode="registry" class="${state.authoringMode === "registry" ? "active" : ""}">Resource Library</button>
      </div>
    </section>
    ${state.authoringMode === "registry" ? registryAuthoringView() : experimentAuthoringView()}
  `;
}

function experimentAuthoringView(): string {
  return `
    <section class="section grid">
      <div class="span-7 panel">
        <div class="section-head">
          <h3>Experiment Design</h3>
          <button type="button" data-action="reset-draft">Reset Form</button>
        </div>
        <form class="form-grid" data-action="draft-preview">
          <label class="field span-3">
            <span class="label">Name</span>
            <input name="experiment_name" value="${draftValue("experiment_name")}" />
          </label>
          <label class="field span-2">
            <span class="label">ID</span>
            <input name="experiment_id" value="${draftValue("experiment_id")}" />
          </label>
          <label class="field">
            <span class="label">Backend</span>
            <select name="compute_backend">
              ${option("local-docker", draftValue("compute_backend"))}
              ${option("runner-docker", draftValue("compute_backend"))}
              ${option("modal", draftValue("compute_backend"))}
            </select>
          </label>
          <label class="field span-6">
            <span class="label">Description</span>
            <input name="description" value="${draftValue("description")}" />
          </label>
          <label class="field span-3">
            <span class="label">Cases path</span>
            <input name="cases_path" value="${draftValue("cases_path")}" />
          </label>
          <label class="field">
            <span class="label">Cases count</span>
            <input name="cases_count" type="number" min="0" placeholder="unknown" value="${draftValue("cases_count")}" />
          </label>
          <label class="field">
            <span class="label">Repeats</span>
            <input name="repeats" type="number" min="1" value="${draftValue("repeats")}" />
          </label>
          <label class="field">
            <span class="label">Concurrency</span>
            <input name="max_concurrency" type="number" min="1" value="${draftValue("max_concurrency")}" />
          </label>
          <label class="field span-3">
            <span class="label">Seeds</span>
            <input name="seeds" value="${draftValue("seeds")}" />
          </label>
          <label class="field span-3">
            <span class="label">Timeout ms</span>
            <input name="timeout_ms" type="number" min="1000" step="1000" value="${draftValue("timeout_ms")}" />
          </label>
          <div class="span-6 form-subsection">
            <div class="subhead">Variants</div>
            ${variantEditorRow(0, { name: "Baseline", id: "baseline", config: "model=gpt-5, mode=baseline", baseline: true })}
            ${variantEditorRow(1, { name: "Treatment", id: "treatment", config: "model=gpt-5, mode=treatment" })}
            ${variantEditorRow(2, { name: "", id: "", config: "", optional: true })}
          </div>
          <div class="span-6 form-subsection">
            <div class="subhead">Metrics</div>
            ${metricEditorRow(0, { name: "Resolved", id: "resolved", direction: "maximize", primary: true })}
            ${metricEditorRow(1, { name: "Keyword hits", id: "keyword_hits", direction: "maximize", optional: true })}
          </div>
          <div class="actions span-6">
            <button type="submit" ${state.loading ? "disabled" : ""}>Preview Experiment</button>
            <button type="button" data-action="draft-validate" ${state.loading ? "disabled" : ""}>Check Draft</button>
            <button type="button" data-action="draft-check-resources" ${state.loading ? "disabled" : ""}>Check Library Reuse</button>
            <button class="primary" type="button" data-action="draft-export" ${state.loading ? "disabled" : ""}>Generate YAML</button>
            <button type="button" data-action="copy-yaml" ${state.draftYaml ? "" : "disabled"}>Copy YAML</button>
          </div>
        </form>
      </div>
      <div class="span-5">
        ${draftStatusPanel()}
      </div>
    </section>
    ${state.draftYaml ? `
      <section class="section panel">
        <div class="section-head"><h3>YAML</h3></div>
        <pre class="yaml-view">${escapeHtml(state.draftYaml)}</pre>
      </section>
    ` : ""}
  `;
}

function registryAuthoringView(): string {
  return `
    <section class="section grid">
      <div class="span-5 panel">
        <div class="section-head">
          <h3>Find Saved Resource</h3>
          <button type="button" data-action="clear-authoring">Clear</button>
        </div>
        <form class="form-grid" data-action="registry-search">
          <label class="field span-2">
            <span class="label">Kind</span>
            ${kindSelect("kind")}
          </label>
          <label class="field span-3">
            <span class="label">Search</span>
            <input name="query" placeholder="baseline, exact id, metric name" />
          </label>
          <div class="field span-5">
            <button class="primary" type="submit" ${state.loading ? "disabled" : ""}>Search Library</button>
          </div>
        </form>
        ${registryHitsTable(state.registryHits)}
      </div>
      <div class="span-7 panel">
        <div class="section-head"><h3>Review Before Saving</h3></div>
        <form class="form-grid" data-action="registry-review">
          <label class="field span-2">
            <span class="label">Kind</span>
            ${kindSelect("kind", "variant")}
          </label>
          <label class="field span-2">
            <span class="label">Schema</span>
            <input name="schema_version" value="v1" />
          </label>
          <label class="field span-2">
            <span class="label">Handle</span>
            <input name="alias" placeholder="baseline-gpt5" />
          </label>
          <label class="field span-6">
            <span class="label">Resource JSON</span>
            <textarea class="json-editor" name="object" spellcheck="false">${escapeHtml(defaultRegistryObjectJson())}</textarea>
          </label>
          <div class="actions span-6">
            <button type="submit" ${state.loading ? "disabled" : ""}>Review</button>
            <button class="primary" type="button" data-action="registry-register" ${state.registryReview ? "" : "disabled"}>Save Reviewed Resource</button>
          </div>
        </form>
        ${registryReviewPanel(state.registryReview)}
      </div>
    </section>
  `;
}

function resourceView(): string {
  const resource = state.selectedResource;
  if (!resource) {
    return `
      <section class="section panel">
        <div class="section-head">
          <h3>Saved Resource</h3>
          <button type="button" data-view="authoring">Back to Library</button>
        </div>
        <div class="empty">Choose a saved resource from the library to inspect it.</div>
      </section>
    `;
  }
  return `
    <section class="section panel resource-hero">
      <div class="section-head">
        <div>
          <h3>${escapeHtml(resourceDisplayName(resource))}</h3>
          <div class="resource-meta">
            ${pill(String(resource.kind ?? ""), "info")}
            ${pill(`schema ${String(resource.schema_version ?? "")}`, "")}
            <span>${dateCell(resource.created_at)}</span>
          </div>
        </div>
        <button type="button" data-view="authoring">Back to Library</button>
      </div>
      <div class="digest-block">
        <div class="label">Stable identity</div>
        <div class="mono">${escapeHtml(String(resource.content_digest ?? ""))}</div>
      </div>
    </section>
    <section class="section grid">
      <div class="span-5 panel">
        <div class="section-head"><h3>Authoring Handles</h3></div>
        ${aliasesTable(state.selectedResourceAliases)}
      </div>
      <div class="span-7 panel">
        <div class="section-head"><h3>Details</h3></div>
        ${resourceFacts(resource)}
      </div>
      <div class="span-12 panel">
        <div class="section-head"><h3>Resource JSON</h3></div>
        <pre class="json-view">${escapeHtml(JSON.stringify(resource.canonical_json ?? {}, null, 2))}</pre>
      </div>
    </section>
  `;
}

function runsView(): string {
  return `
    <section class="section panel">
      <div class="section-head">
        <h3>Run Experiment</h3>
        <button type="button" data-action="refresh" ${state.loading ? "disabled" : ""}>Refresh</button>
      </div>
      <form class="form-grid" data-action="create-run">
        <label class="field span-3">
          <span class="label">Experiment build</span>
          <select name="package_digest">
            ${state.packages.map((pkg) => `<option value="${escapeAttr(String(pkg.package_digest ?? ""))}">${escapeHtml(packageTitle(pkg))}</option>`).join("")}
          </select>
        </label>
        <label class="field span-2">
          <span class="label">Label</span>
          <input name="run_label" placeholder="first-cloud-smoke" />
        </label>
        <label class="field">
          <span class="label">Backend</span>
          <select name="backend">
            <option value="runner-docker">Runner Docker</option>
            <option value="modal">Modal</option>
          </select>
        </label>
        <label class="field">
          <span class="label">Arch</span>
          <select name="arch">
            <option value="x86_64">x86_64</option>
            <option value="arm64">arm64</option>
          </select>
        </label>
        <label class="field">
          <span class="label">CPU</span>
          <input name="cpu_count" type="number" min="1" value="2" />
        </label>
        <label class="field">
          <span class="label">Memory MB</span>
          <input name="memory_mb" type="number" min="512" step="512" value="4096" />
        </label>
        <label class="field">
          <span class="label">Disk MB</span>
          <input name="disk_mb" type="number" min="10240" step="1024" value="20480" />
        </label>
        <label class="field">
          <span class="label">Isolation</span>
          <select name="isolation">
            <option value="reusable_vm">Reusable VM</option>
            <option value="single_use_vm">Single-use VM</option>
          </select>
        </label>
        <label class="field span-3">
          <span class="label">Env JSON</span>
          <textarea name="env" placeholder='{"OPENAI_BASE_URL":"https://api.openai.com"}'></textarea>
        </label>
        <label class="field span-3">
          <span class="label">Secret refs JSON</span>
          <textarea name="secret_refs" placeholder='{"OPENAI_API_KEY":"provider/openai"}'></textarea>
        </label>
        <div class="field span-6">
          <button class="primary" type="submit" ${state.loading ? "disabled" : ""}>Start Experiment</button>
        </div>
      </form>
    </section>
    <section class="section panel">
      <div class="section-head"><h3>Experiment History</h3></div>
      ${runsTable(state.runs)}
    </section>
  `;
}

function runnersView(): string {
  return `
    <section class="section grid">
      <div class="span-6 panel">
        <div class="section-head">
          <h3>Execution Service</h3>
          <button type="button" data-action="refresh" ${state.loading ? "disabled" : ""}>Refresh</button>
        </div>
        ${managedServiceTable()}
      </div>
      <div class="span-6 panel">
        <div class="section-head">
          <h3>Workers</h3>
          <button type="button" data-action="expire-stale" ${state.loading ? "disabled" : ""}>Mark stale offline</button>
        </div>
        ${instancesTable(state.instances)}
      </div>
      <div class="span-12 panel">
        <div class="section-head"><h3>Worker Startup</h3></div>
        ${provisionsTable(state.provisions)}
      </div>
    </section>
  `;
}

function metric(label: string, value: string, tone: string): string {
  return `<div class="span-4 panel metric">
    <span class="label">${escapeHtml(label)}</span>
    <span class="value">${escapeHtml(value)}</span>
    <span class="pill ${tone}">${escapeHtml(tone)}</span>
  </div>`;
}

function packagesTable(rows: Json[]): string {
  if (rows.length === 0) {
    return `<div class="empty">No builds uploaded yet.</div>`;
  }
  return table(["Build", "Status", "Images", "Updated"], rows.map((pkg) => [
    entityTitle(packageTitle(pkg), String(pkg.package_digest ?? "")),
    pill(String(pkg.status ?? ""), toneForStatus(String(pkg.status ?? ""))),
    list((pkg.image_refs as string[] | undefined) ?? []),
    dateCell(pkg.updated_at),
  ]));
}

function packageCards(rows: Json[]): string {
  if (rows.length === 0) {
    return `<div class="empty">No builds uploaded yet.</div>`;
  }
  return `<div class="resource-list">${rows.map((pkg) => `
    <article class="resource-row">
      <div class="resource-main">
        <div class="resource-title">${escapeHtml(packageTitle(pkg))}</div>
        <div class="resource-meta">
          ${pill(String(pkg.status ?? ""), toneForStatus(String(pkg.status ?? "")))}
          ${pill(`${String(((pkg.image_refs as string[] | undefined) ?? []).length)} images`, "")}
          <span>${dateCell(pkg.updated_at)}</span>
        </div>
      </div>
      <div class="resource-side">
        <div class="label">Stable ID</div>
        ${mono(shortDigest(String(pkg.package_digest ?? "")))}
      </div>
    </article>
  `).join("")}</div>`;
}

function importsTable(rows: Json[]): string {
  if (rows.length === 0) {
    return `<div class="empty">No uploads yet.</div>`;
  }
  return table(["Upload", "Status", "Build", "Diagnostics"], rows.map((job) => [
    entityTitle(String(job.label ?? "") || "Upload", String(job.import_id ?? "")),
    pill(String(job.status ?? ""), toneForStatus(String(job.status ?? ""))),
    mono(shortDigest(String(job.package_digest ?? ""))),
    String((job.diagnostics as unknown[] | undefined)?.length ?? 0),
  ]));
}

function runsTable(rows: Json[]): string {
  if (rows.length === 0) {
    return `<div class="empty">No experiments started yet.</div>`;
  }
  return table(["Experiment", "Status", "Label", "Execution Shape", "Created"], rows.map((run) => [
    mono(String(run.run_id ?? "")),
    pill(String(run.status ?? ""), toneForStatus(String(run.status ?? ""))),
    escapeHtml(String(run.run_label ?? "")),
    requirementsCell(run.run_requirements as Json | undefined),
    dateCell(run.created_at),
  ]));
}

function managedServiceTable(): string {
  const pool = managedPool();
  if (!pool) {
    return `<div class="empty">Execution service is not configured.</div>`;
  }
  return table(["Service", "Status", "Current Shape"], [[
    `<div>${escapeHtml(String(pool.name ?? "managed-runner-service"))}</div><div class="mono">${escapeHtml(String(pool.runner_pool_id ?? ""))}</div>`,
    pill(String(pool.status ?? ""), toneForStatus(String(pool.status ?? ""))),
    capabilitiesCell(pool.capabilities as Json | undefined),
  ]]);
}

function instancesTable(rows: Json[]): string {
  if (rows.length === 0) {
    return `<div class="empty">No workers are online.</div>`;
  }
  return table(["Worker", "Status", "Last Seen"], rows.map((instance) => [
    `<div>${escapeHtml(String(instance.instance_name ?? ""))}</div><div class="mono">${escapeHtml(String(instance.runner_instance_id ?? ""))}</div>`,
    runnerStatusPill(instance),
    `<div>${dateCell(instance.last_heartbeat_at)}</div><div class="label">${escapeHtml(heartbeatAge(instance.last_heartbeat_at))}</div>`,
  ]));
}

function provisionsTable(rows: Json[]): string {
  if (rows.length === 0) {
    return `<div class="empty">No worker startup requests.</div>`;
  }
  return table(["Startup", "Status", "Experiment", "Cloud VM", "Updated"], rows.map((request) => [
    mono(String(request.provision_request_id ?? "")),
    pill(String(request.status ?? ""), toneForStatus(String(request.status ?? ""))),
    mono(String(request.run_id ?? "")),
    mono(String(request.provider_instance_id ?? "")),
    dateCell(request.updated_at),
  ]));
}

function registryHitsTable(rows: Json[]): string {
  if (rows.length === 0) {
    return `<div class="empty">No saved resources found.</div>`;
  }
  return table(["Resource", "Kind", "Handles"], rows.map((hit) => [
    resourceLink(String(hit.display_name ?? ""), String(hit.content_digest ?? "")),
    pill(String(hit.kind ?? ""), "info"),
    list(((hit.aliases as string[] | undefined) ?? []).map(String)),
  ]));
}

function registryReviewPanel(review: Json | null): string {
  if (!review) {
    return `<div class="empty">Review a resource to see its stable identity, handle status, and similar saved resources.</div>`;
  }
  const canonical = isJson(review.canonical) ? review.canonical : {};
  const exact = isJson(review.exact_match) ? review.exact_match : {};
  const aliases = arrayFrom(review.alias_reviews);
  const similar = arrayFrom(review.similar);
  const suggestions = arrayFrom(review.suggested_actions);
  return `
    <div class="review-panel">
      <div class="review-summary">
        ${pill(exact.exists ? "already saved" : "new resource", exact.exists ? "ok" : "info")}
        ${exact.exists ? resourceLink(String(canonical.kind ?? ""), String(canonical.content_digest ?? "")) : entityTitle(String(canonical.kind ?? ""), String(canonical.content_digest ?? ""))}
      </div>
      ${aliases.length > 0 ? table(["Handle", "Status", "Existing"], aliases.map((alias) => [
        escapeHtml(String(alias.alias ?? "")),
        pill(handleStatusLabel(String(alias.status ?? "")), toneForAlias(String(alias.status ?? ""))),
        mono(shortDigest(String(alias.existing_digest ?? ""))),
      ])) : `<div class="empty">No handle requested.</div>`}
      ${similar.length > 0 ? `<div class="subhead">Similar Saved Resources</div>${registryHitsTable(similar)}` : ""}
      ${suggestions.length > 0 ? `<div class="subhead">Next Actions</div>${table(["Action", "Reason"], suggestions.map((action) => [
        pill(actionLabel(String(action.action ?? "")), ""),
        escapeHtml(String(action.reason ?? "")),
      ]))}` : ""}
    </div>
  `;
}

function variantEditorRow(index: number, defaults: {
  name: string;
  id: string;
  config: string;
  baseline?: boolean;
  optional?: boolean;
}): string {
  return `
    <div class="editor-row">
      <label class="check-field">
        <input type="checkbox" name="variant_${index}_enabled" ${draftChecked(`variant_${index}_enabled`, defaults.optional ? "" : "on")} />
        <span>${escapeHtml(defaults.optional ? "Use" : defaults.baseline ? "Baseline" : "Use")}</span>
      </label>
      <label class="field">
        <span class="label">Source</span>
        <select name="variant_${index}_source">
          ${option("inline", draftRaw(`variant_${index}_source`) || "inline")}
          ${option("alias", draftRaw(`variant_${index}_source`) || "inline", "Saved handle")}
        </select>
      </label>
      <label class="field">
        <span class="label">Name</span>
        <input name="variant_${index}_name" value="${draftValue(`variant_${index}_name`, defaults.name)}" />
      </label>
      <label class="field">
        <span class="label">ID</span>
        <input name="variant_${index}_id" value="${draftValue(`variant_${index}_id`, defaults.id)}" />
      </label>
      <label class="field">
        <span class="label">Handle</span>
        <input name="variant_${index}_alias" placeholder="baseline-gpt5" value="${draftValue(`variant_${index}_alias`)}" />
      </label>
      <label class="field wide-field">
        <span class="label">Config</span>
        <input name="variant_${index}_config" value="${draftValue(`variant_${index}_config`, defaults.config)}" />
      </label>
      <label class="check-field">
        <input type="checkbox" name="variant_${index}_baseline" ${draftChecked(`variant_${index}_baseline`, defaults.baseline ? "on" : "")} />
        <span>Baseline</span>
      </label>
    </div>
  `;
}

function metricEditorRow(index: number, defaults: {
  name: string;
  id: string;
  direction: string;
  primary?: boolean;
  optional?: boolean;
}): string {
  return `
    <div class="editor-row metric-row">
      <label class="check-field">
        <input type="checkbox" name="metric_${index}_enabled" ${draftChecked(`metric_${index}_enabled`, defaults.optional ? "" : "on")} />
        <span>Use</span>
      </label>
      <label class="field">
        <span class="label">Name</span>
        <input name="metric_${index}_name" value="${draftValue(`metric_${index}_name`, defaults.name)}" />
      </label>
      <label class="field">
        <span class="label">ID</span>
        <input name="metric_${index}_id" value="${draftValue(`metric_${index}_id`, defaults.id)}" />
      </label>
      <label class="field">
        <span class="label">Direction</span>
        <select name="metric_${index}_direction">
          ${option("maximize", draftRaw(`metric_${index}_direction`) || defaults.direction)}
          ${option("minimize", draftRaw(`metric_${index}_direction`) || defaults.direction)}
        </select>
      </label>
      <label class="check-field">
        <input type="checkbox" name="metric_${index}_primary" ${draftChecked(`metric_${index}_primary`, defaults.primary ? "on" : "")} />
        <span>Primary</span>
      </label>
    </div>
  `;
}

function draftStatusPanel(): string {
  const preview = state.draftPreview;
  const validation = state.draftValidation;
  const issues = arrayFrom(validation?.issues ?? preview?.warnings);
  const refs = arrayFrom(validation?.resolved_refs);
  return `
    <div class="panel sticky-panel">
      <div class="section-head"><h3>Experiment Readiness</h3></div>
      ${preview ? draftPreviewSummary(preview) : draftPreviewEmpty()}
      <div class="draft-metrics">
        ${draftMetric("Slots", preview?.total_slots ?? "unknown")}
        ${draftMetric("Variants", preview?.variants ?? "0")}
        ${draftMetric("Repeats", preview?.repeats ?? "0")}
        ${draftMetric("Concurrency", preview?.max_concurrency ?? "unset")}
      </div>
      <div class="subhead">Needs Attention</div>
      ${issues.length === 0 ? `<div class="empty compact-empty">No issues yet.</div>` : issuesList(issues)}
      <div class="subhead">Linked Library Items</div>
      ${refs.length === 0 ? `<div class="empty compact-empty">No saved resources linked yet.</div>` : resolvedRefsList(refs)}
      <div class="subhead">Library Reuse</div>
      ${state.draftResourceReviews.length === 0 ? `<div class="empty compact-empty">Check library reuse to compare draft resources against saved resources.</div>` : resourceReviewList(state.draftResourceReviews)}
    </div>
  `;
}

function draftPreviewEmpty(): string {
  return `
    <div class="preview-card muted-card">
      <div class="entity-title">No preview yet</div>
      <div class="resource-check-message">Preview will show the experiment shape, trial math, and anything that can only be known when the build runs.</div>
    </div>
  `;
}

function draftPreviewSummary(preview: Json): string {
  const cases = preview.cases === null || preview.cases === undefined ? "cases counted during build" : `${String(preview.cases)} cases`;
  const slots = preview.total_slots === null || preview.total_slots === undefined
    ? "Open-ended until cases are counted"
    : `${String(preview.total_slots)} trial slots`;
  return `
    <div class="preview-card">
      <div class="preview-kicker">Preview ready</div>
      <div class="preview-title">${escapeHtml(slots)}</div>
      ${state.draftDirty ? `<div class="pill warn">changed since preview</div>` : ""}
      <div class="preview-formula">
        ${escapeHtml(String(preview.variants ?? 0))} variants x ${escapeHtml(cases)} x ${escapeHtml(String(preview.repeats ?? 1))} repeats
      </div>
      <div class="resource-meta">
        ${pill(`${String(preview.seeds ?? 1)} seed${preview.seeds === 1 ? "" : "s"}`, "")}
        ${pill(`concurrency ${String(preview.max_concurrency ?? "unset")}`, "")}
      </div>
    </div>
  `;
}

function draftMetric(label: string, value: unknown): string {
  return `<div class="draft-metric"><span class="label">${escapeHtml(label)}</span><strong>${escapeHtml(String(value ?? "unknown"))}</strong></div>`;
}

function issuesList(rows: Json[]): string {
  return `<div class="issue-list">${rows.map((issue) => `
    <div class="issue-row ${escapeAttr(String(issue.severity ?? ""))}">
      ${pill(String(issue.severity ?? "info"), toneForIssue(String(issue.severity ?? "")))}
      <div>
        <div class="entity-title">${escapeHtml(String(issue.message ?? ""))}</div>
        <div class="mono subdued">${escapeHtml(String(issue.pointer ?? issue.code ?? ""))}</div>
      </div>
    </div>
  `).join("")}</div>`;
}

function resolvedRefsList(rows: Json[]): string {
  return `<div class="resource-list">${rows.map((ref) => `
    <div class="mini-resource">
      <div>
        <div class="entity-title">${escapeHtml(String(ref.display_name ?? ref.kind ?? "Resource"))}</div>
        <div class="resource-meta">${pill(String(ref.kind ?? ""), "info")} ${pill(String(ref.resolution ?? ""), "")}</div>
      </div>
      <div class="mono subdued">${escapeHtml(shortDigest(String(ref.content_digest ?? "")))}</div>
    </div>
  `).join("")}</div>`;
}

function resourceReviewList(rows: Json[]): string {
  return `<div class="resource-list">${rows.map((row) => {
    const similar = arrayFrom(row.similar);
    return `
      <div class="resource-check ${escapeAttr(String(row.status ?? ""))}">
        <div>
          <div class="entity-title">${escapeHtml(String(row.label ?? "Resource"))}</div>
          <div class="resource-meta">
            ${pill(String(row.kind ?? ""), "info")}
            ${pill(statusLabel(String(row.status ?? "")), toneForResourceReview(String(row.status ?? "")))}
            ${row.digest ? `<span class="mono subdued">${escapeHtml(shortDigest(String(row.digest)))}</span>` : ""}
          </div>
        </div>
        ${row.message ? `<div class="resource-check-message">${escapeHtml(String(row.message))}</div>` : ""}
        ${similar.length > 0 ? `<div class="similar-list">${similar.slice(0, 3).map((hit) => resourceLink(String(hit.display_name ?? ""), String(hit.content_digest ?? ""))).join("")}</div>` : ""}
        ${row.review ? `<div><button type="button" data-open-resource-review="${escapeAttr(String(row.index ?? ""))}">Review Save</button></div>` : ""}
      </div>
    `;
  }).join("")}</div>`;
}

function table(headers: string[], rows: string[][]): string {
  return `<div class="table-wrap"><table><thead><tr>${headers.map((header) => `<th>${escapeHtml(header)}</th>`).join("")}</tr></thead><tbody>${rows.map((row) => `<tr>${row.map((cell) => `<td>${cell}</td>`).join("")}</tr>`).join("")}</tbody></table></div>`;
}

function aliasesTable(rows: Json[]): string {
  if (rows.length === 0) {
    return `<div class="empty">No handles saved.</div>`;
  }
  return table(["Handle", "Scope", "Created"], rows.map((alias) => [
    escapeHtml(String(alias.alias ?? "")),
    escapeHtml(`${String(alias.scope_type ?? "global")}${alias.scope_id ? `/${String(alias.scope_id)}` : ""}`),
    dateCell(alias.created_at),
  ]));
}

function resourceFacts(resource: Json): string {
  const canonical = isJson(resource.canonical_json) ? resource.canonical_json : {};
  const rows = [
    ["Kind", pill(String(resource.kind ?? ""), "info")],
    ["Schema", escapeHtml(String(resource.schema_version ?? ""))],
    ["Saved", dateCell(resource.created_at)],
    ["Size", escapeHtml(`${String(resource.canonical_size_bytes ?? "?")} bytes`)],
    ["Source", escapeHtml(String(resource.source_uri ?? "none"))],
    ["Display name", escapeHtml(resourceDisplayName(resource))],
    ["Object id", escapeHtml(String(canonical.id ?? "none"))],
  ];
  return table(["Field", "Value"], rows);
}

function bindEvents(): void {
  document.querySelectorAll<HTMLButtonElement>("[data-view]").forEach((button) => {
    button.addEventListener("click", () => {
      state.activeView = button.dataset.view as AppState["activeView"];
      state.error = "";
      state.message = "";
      render();
    });
  });

  document.querySelectorAll<HTMLButtonElement>("[data-authoring-mode]").forEach((button) => {
    button.addEventListener("click", () => {
      state.authoringMode = button.dataset.authoringMode as AppState["authoringMode"];
      state.error = "";
      state.message = "";
      render();
    });
  });

  document.querySelector<HTMLFormElement>('[data-action="settings"]')?.addEventListener("submit", (event) => {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    state.apiBase = String(form.get("apiBase") ?? "").replace(/\/+$/, "");
    state.userToken = String(form.get("userToken") ?? "");
    state.workerToken = String(form.get("workerToken") ?? "");
    state.runnerPoolId = String(form.get("runnerPoolId") ?? "").trim();
    localStorage.setItem("buc.apiBase", state.apiBase);
    localStorage.setItem("buc.userToken", state.userToken);
    localStorage.setItem("buc.workerToken", state.workerToken);
    localStorage.setItem("buc.runnerPoolId", state.runnerPoolId);
    state.message = "Settings saved.";
    void refreshAll();
  });

  document.querySelectorAll<HTMLButtonElement>('[data-action="refresh"]').forEach((button) => {
    button.addEventListener("click", () => void refreshAll());
  });

  document.querySelectorAll<HTMLButtonElement>('[data-action="expire-stale"]').forEach((button) => {
    button.addEventListener("click", () => void expireStaleInstances());
  });

  document.querySelector<HTMLFormElement>('[data-action="create-run"]')?.addEventListener("submit", (event) => {
    event.preventDefault();
    void createRun(event.currentTarget);
  });

  document.querySelector<HTMLFormElement>('[data-action="import-package"]')?.addEventListener("submit", (event) => {
    event.preventDefault();
    void importPackage(event.currentTarget);
  });

  document.querySelector<HTMLFormElement>('[data-action="registry-search"]')?.addEventListener("submit", (event) => {
    event.preventDefault();
    void searchRegistry(event.currentTarget);
  });

  document.querySelector<HTMLFormElement>('[data-action="draft-preview"]')?.addEventListener("submit", (event) => {
    event.preventDefault();
    void previewDraft(event.currentTarget);
  });

  document.querySelector<HTMLButtonElement>('[data-action="draft-validate"]')?.addEventListener("click", (event) => {
    const form = event.currentTarget.closest("form");
    if (form instanceof HTMLFormElement) {
      void validateDraft(form);
    }
  });

  document.querySelector<HTMLButtonElement>('[data-action="draft-export"]')?.addEventListener("click", (event) => {
    const form = event.currentTarget.closest("form");
    if (form instanceof HTMLFormElement) {
      void exportDraft(form);
    }
  });

  document.querySelector<HTMLButtonElement>('[data-action="draft-check-resources"]')?.addEventListener("click", (event) => {
    const form = event.currentTarget.closest("form");
    if (form instanceof HTMLFormElement) {
      void checkDraftResources(form);
    }
  });

  document.querySelector<HTMLButtonElement>('[data-action="copy-yaml"]')?.addEventListener("click", () => {
    void copyYaml();
  });

  document.querySelector<HTMLButtonElement>('[data-action="reset-draft"]')?.addEventListener("click", () => {
    state.draftForm = defaultDraftForm();
    state.draftPreview = null;
    state.draftValidation = null;
    state.draftYaml = "";
    state.draftResourceReviews = [];
    state.draftDirty = false;
    state.message = "";
    state.error = "";
    render();
  });

  document.querySelector<HTMLFormElement>('[data-action="draft-preview"]')?.addEventListener("input", (event) => {
    if (event.currentTarget instanceof HTMLFormElement) {
      captureDraftForm(event.currentTarget);
      state.draftDirty = true;
    }
  });

  document.querySelector<HTMLFormElement>('[data-action="registry-review"]')?.addEventListener("submit", (event) => {
    event.preventDefault();
    void reviewRegistryObject(event.currentTarget);
  });

  document.querySelector<HTMLButtonElement>('[data-action="registry-register"]')?.addEventListener("click", () => {
    void registerReviewedObject();
  });

  document.querySelector<HTMLButtonElement>('[data-action="clear-authoring"]')?.addEventListener("click", () => {
    state.registryHits = [];
    state.registryReview = null;
    state.selectedResource = null;
    state.selectedResourceAliases = [];
    state.message = "";
    state.error = "";
    render();
  });

  document.querySelectorAll<HTMLButtonElement>("[data-resource-digest]").forEach((button) => {
    button.addEventListener("click", () => {
      const digest = button.dataset.resourceDigest;
      if (digest) {
        void openResource(digest);
      }
    });
  });

  document.querySelectorAll<HTMLButtonElement>("[data-open-resource-review]").forEach((button) => {
    button.addEventListener("click", () => {
      const index = Number.parseInt(button.dataset.openResourceReview ?? "", 10);
      if (Number.isFinite(index)) {
        openDraftResourceReview(index);
      }
    });
  });
}

async function refreshAll(): Promise<void> {
  await withLoading(async () => {
    const [health, packages, imports, runs] = await Promise.all([
      apiGet("/readyz").catch((error) => ({ error: messageForError(error) })),
      apiGet("/v1/packages?limit=100").catch(() => ({ packages: [] })),
      apiGet("/v1/imports?limit=100").catch(() => ({ imports: [] })),
      apiGet("/v1/runs?limit=100").catch(() => ({ runs: [] })),
    ]);
    state.health = health;
    state.packages = arrayFrom(packages.packages);
    state.imports = arrayFrom(imports.imports);
    state.runs = arrayFrom(runs.runs);

    if (state.workerToken) {
      const listQuery = state.runnerPoolId ? `?runner_pool_id=${encodeURIComponent(state.runnerPoolId)}&limit=200` : "?limit=200";
      const [pools, instances, provisions] = await Promise.all([
        apiGet("/v1/runner-pools", "worker").catch(() => ({ runner_pools: [] })),
        apiGet(`/v1/runner-instances${listQuery}`, "worker").catch(() => ({ runner_instances: [] })),
        apiGet(`/v1/runner-provision-requests${listQuery}`, "worker").catch(() => ({ provision_requests: [] })),
      ]);
      state.pools = scopedPools(arrayFrom(pools.runner_pools));
      state.instances = arrayFrom(instances.runner_instances);
      state.provisions = arrayFrom(provisions.provision_requests);
    }
  });
}

async function createRun(formElement: HTMLFormElement): Promise<void> {
  await withLoading(async () => {
    const form = new FormData(formElement);
    const env = parseJsonObject(String(form.get("env") ?? "") || "{}", "Env JSON");
    const secretRefs = parseJsonObject(String(form.get("secret_refs") ?? "") || "{}", "Secret refs JSON");
    const body = {
      package_digest: String(form.get("package_digest") ?? ""),
      run_label: String(form.get("run_label") ?? "") || null,
      env,
      secret_refs: secretRefs,
      runtime_options: {
        backend: String(form.get("backend") ?? "runner-docker"),
        arch: String(form.get("arch") ?? "x86_64"),
        isolation: String(form.get("isolation") ?? "reusable_vm"),
        cpu_count: numberFromForm(form, "cpu_count"),
        memory_mb: numberFromForm(form, "memory_mb"),
        disk_mb: numberFromForm(form, "disk_mb"),
      },
    };
    const run = await apiPost("/v1/runs", body);
    state.message = `Experiment queued: ${String(run.run_id ?? "")}`;
    await refreshAll();
  });
}

async function importPackage(formElement: HTMLFormElement): Promise<void> {
  await withLoading(async () => {
    const form = new FormData(formElement);
    const file = form.get("file");
    if (!(file instanceof File) || file.size === 0) {
      throw new Error("Choose a build archive.");
    }
    const upload = await apiPost("/v1/uploads", {
      filename: file.name,
      media_type: file.type || "application/gzip",
      byte_size: file.size,
    });
    const uploadId = String(upload.upload_id ?? "");
    await apiRaw(`/v1/uploads/${encodeURIComponent(uploadId)}/content`, "PUT", file, file.type || "application/octet-stream");
    await apiPost(`/v1/uploads/${encodeURIComponent(uploadId)}/complete`, {});
    const imported = await apiPost("/v1/imports/sealed-package", {
      upload_id: uploadId,
      label: String(form.get("label") ?? "") || null,
    });
    state.message = `Upload ${String(imported.import_id ?? "")}: ${String(imported.status ?? "")}`;
    await refreshAll();
  });
}

async function expireStaleInstances(): Promise<void> {
  await withLoading(async () => {
    const result = await apiPost("/v1/runner-instances/expire-stale", {
      stale_after_seconds: 90,
      ...(state.runnerPoolId ? { runner_pool_id: state.runnerPoolId } : {}),
    }, "worker");
    const count = arrayFrom(result.runner_instances).length;
    state.message = `Marked ${count} stale worker${count === 1 ? "" : "s"} offline.`;
    await refreshAll();
  });
}

async function previewDraft(formElement: HTMLFormElement): Promise<void> {
  captureDraftForm(formElement);
  const draft = draftFromForm(formElement);
  await withLoading(async () => {
    state.draftPreview = await apiPost("/v1/drafts/preview-schedule", { draft });
    state.draftValidation = null;
    state.draftYaml = "";
    state.draftDirty = false;
    state.message = previewMessage(state.draftPreview);
  });
}

async function validateDraft(formElement: HTMLFormElement): Promise<void> {
  captureDraftForm(formElement);
  const draft = draftFromForm(formElement);
  await withLoading(async () => {
    const [preview, validation] = await Promise.all([
      apiPost("/v1/drafts/preview-schedule", { draft }),
      apiPost("/v1/drafts/validate", { draft }),
    ]);
    state.draftPreview = preview;
    state.draftValidation = validation;
    state.draftYaml = "";
    state.draftDirty = false;
    state.message = validation.valid ? "Draft is ready." : "Draft needs attention.";
  });
}

async function exportDraft(formElement: HTMLFormElement): Promise<void> {
  captureDraftForm(formElement);
  const draft = draftFromForm(formElement);
  await withLoading(async () => {
    const [preview, validation, exported] = await Promise.all([
      apiPost("/v1/drafts/preview-schedule", { draft }),
      apiPost("/v1/drafts/validate", { draft }),
      apiPost("/v1/drafts/export", { draft, format: "yaml" }),
    ]);
    state.draftPreview = preview;
    state.draftValidation = validation;
    state.draftYaml = String(exported.body ?? "");
    state.draftDirty = false;
    state.message = validation.valid ? "YAML generated." : "YAML generated with issues.";
  });
}

async function checkDraftResources(formElement: HTMLFormElement): Promise<void> {
  captureDraftForm(formElement);
  const resources = draftResourcesFromForm(new FormData(formElement));
  await withLoading(async () => {
    const reviews = await Promise.all(resources.map(async (resource) => {
      if (resource.source === "alias") {
        return await reviewAliasResource(resource);
      }
      return await reviewInlineResource(resource);
    }));
    state.draftResourceReviews = reviews.map((review, index) => ({ ...review, index }));
    state.draftDirty = false;
    state.message = `Checked ${reviews.length} draft resource${reviews.length === 1 ? "" : "s"} against the library.`;
  });
}

async function copyYaml(): Promise<void> {
  if (!state.draftYaml) {
    return;
  }
  await navigator.clipboard.writeText(state.draftYaml);
  state.message = "YAML copied.";
  render();
}

async function reviewAliasResource(resource: DraftResource): Promise<Json> {
  const result = await apiPost("/v1/registry/resolve", {
    ref: {
      kind: resource.kind,
      alias: resource.alias,
      schema_version: `${resource.kind}_v1`,
    },
  }).catch((error) => ({ error: messageForError(error) }));
  if (typeof result.error === "string") {
    return {
      label: resource.label,
      kind: resource.kind,
      source: "alias",
      status: "missing",
      message: result.error,
    };
  }
  return {
    label: resource.label,
    kind: resource.kind,
    source: "alias",
    status: "reused",
    digest: result.content_digest,
    message: `Handle ${resource.alias} resolves to a saved ${resource.kind}.`,
  };
}

async function reviewInlineResource(resource: DraftResource): Promise<Json> {
  const result = await apiPost("/v1/registry/review", {
    kind: resource.kind,
    schema_version: `${resource.kind}_v1`,
    object: resource.object,
    aliases: resource.alias ? [resource.alias] : [],
  });
  const exact = isJson(result.exact_match) ? result.exact_match : {};
  const canonical = isJson(result.canonical) ? result.canonical : {};
  const aliases = arrayFrom(result.alias_reviews);
  const hasAliasConflict = aliases.some((alias) => alias.status === "conflicts");
  const similar = arrayFrom(result.similar);
  const exists = exact.exists === true;
  return {
    label: resource.label,
    kind: resource.kind,
    source: "inline",
    status: hasAliasConflict ? "conflict" : exists ? "reused" : similar.length > 0 ? "similar" : "new",
    digest: canonical.content_digest,
    similar,
    review: result,
    message: hasAliasConflict
      ? "Handle conflicts with another saved resource."
      : exists
        ? "This exact content is already saved."
        : similar.length > 0
          ? "Similar saved resources exist. Inspect before saving."
          : "This looks new. Saving it is still explicit.",
  };
}

async function searchRegistry(formElement: HTMLFormElement): Promise<void> {
  await withLoading(async () => {
    const form = new FormData(formElement);
    const kind = String(form.get("kind") ?? "");
    const query = String(form.get("query") ?? "").trim();
    if (!query) {
      throw new Error("Search requires a query.");
    }
    const params = new URLSearchParams({ kind, q: query, limit: "25" });
    const result = await apiGet(`/v1/registry/search?${params.toString()}`);
    state.registryHits = arrayFrom(result.hits);
    state.message = `Found ${state.registryHits.length} saved resource${state.registryHits.length === 1 ? "" : "s"}.`;
  });
}

async function reviewRegistryObject(formElement: HTMLFormElement): Promise<void> {
  await withLoading(async () => {
    const form = new FormData(formElement);
    const alias = String(form.get("alias") ?? "").trim();
    const body = {
      kind: String(form.get("kind") ?? "variant"),
      schema_version: String(form.get("schema_version") ?? "v1") || "v1",
      object: parseAnyJsonObject(String(form.get("object") ?? "{}"), "Resource JSON"),
      aliases: alias ? [alias] : [],
    };
    state.registryReview = await apiPost("/v1/registry/review", body);
    state.message = "Review complete. Nothing was saved.";
  });
}

async function registerReviewedObject(): Promise<void> {
  const review = state.registryReview;
  if (!review || !isJson(review.canonical)) {
    throw new Error("Review a resource before saving it.");
  }
  await withLoading(async () => {
    const canonical = review.canonical as Json;
    const aliases = arrayFrom(review.alias_reviews)
      .filter((alias) => ["available", "already_points_here"].includes(String(alias.status)))
      .map((alias) => ({
        alias: String(alias.alias ?? ""),
        scope_type: String(alias.scope_type ?? "global"),
        scope_id: alias.scope_id ?? null,
      }))
      .filter((alias) => alias.alias.length > 0);
    const result = await apiPost("/v1/registry/objects", {
      kind: String(canonical.kind ?? ""),
      schema_version: String(canonical.schema_version ?? "v1"),
      canonical_json: canonical.canonical_json,
      expected_digest: String(canonical.content_digest ?? ""),
      aliases,
    });
    const object = isJson(result.object) ? result.object : {};
    state.message = `Saved ${shortDigest(String(object.content_digest ?? canonical.content_digest ?? ""))}.`;
    state.selectedResource = object;
    state.selectedResourceAliases = arrayFrom(result.aliases);
    state.activeView = "resource";
    await refreshAll();
  });
}

async function openResource(digest: string): Promise<void> {
  await withLoading(async () => {
    const result = await apiGet(`/v1/registry/objects/${encodeURIComponent(digest)}`);
    state.selectedResource = isJson(result.object) ? result.object : result;
    state.selectedResourceAliases = arrayFrom(result.aliases);
    state.activeView = "resource";
  });
}

function openDraftResourceReview(index: number): void {
  const review = state.draftResourceReviews[index];
  if (!review || !isJson(review.review)) {
    return;
  }
  state.registryReview = review.review;
  state.registryHits = arrayFrom(review.similar);
  state.authoringMode = "registry";
  state.message = "Opened review. Saving is still explicit.";
  render();
}

async function withLoading(work: () => Promise<void>): Promise<void> {
  state.loading = true;
  state.error = "";
  render();
  try {
    await work();
  } catch (error) {
    state.error = messageForError(error);
  } finally {
    state.loading = false;
    render();
  }
}

type AuthMode = "user" | "worker" | "none";

interface DraftResource {
  kind: string;
  label: string;
  source: "inline" | "alias";
  alias?: string;
  object?: Json;
}

async function apiGet(path: string, auth: AuthMode = "user"): Promise<Json> {
  return apiRequest(path, { method: "GET", auth });
}

async function apiPost(path: string, body: unknown, auth: AuthMode = "user"): Promise<Json> {
  return apiRequest(path, { method: "POST", body, auth });
}

async function apiRaw(path: string, method: string, body: BodyInit, contentType: string): Promise<Json> {
  return apiRequest(path, { method, rawBody: body, contentType, auth: "user" });
}

async function apiRequest(
  path: string,
  options: { method: string; body?: unknown; rawBody?: BodyInit; contentType?: string; auth?: AuthMode },
): Promise<Json> {
  const headers: Record<string, string> = {};
  let body: BodyInit | undefined;
  if (options.rawBody) {
    body = options.rawBody;
    headers["content-type"] = options.contentType ?? "application/octet-stream";
  } else if (options.body !== undefined) {
    body = JSON.stringify(options.body);
    headers["content-type"] = "application/json";
  }
  const authMode = options.auth ?? "user";
  if (authMode === "user" && state.userToken) {
    headers.authorization = `Bearer ${state.userToken}`;
  }
  if (authMode === "worker" && state.workerToken) {
    headers.authorization = `Bearer ${state.workerToken}`;
  }
  const response = await fetch(`${state.apiBase}${path}`, {
    method: options.method,
    headers,
    body,
  });
  const text = await response.text();
  const parsed = text ? JSON.parse(text) : {};
  if (!response.ok) {
    throw new Error(String(parsed.message ?? parsed.code ?? response.statusText));
  }
  return parsed;
}

function parseJsonObject(raw: string, label: string): Record<string, string> {
  const parsed = JSON.parse(raw || "{}");
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`${label} must be an object.`);
  }
  for (const [key, value] of Object.entries(parsed)) {
    if (typeof value !== "string") {
      throw new Error(`${label} value for ${key} must be a string.`);
    }
  }
  return parsed as Record<string, string>;
}

function parseAnyJsonObject(raw: string, label: string): Json {
  const parsed = JSON.parse(raw || "{}");
  if (!isJson(parsed)) {
    throw new Error(`${label} must be an object.`);
  }
  return parsed;
}

function captureDraftForm(formElement: HTMLFormElement): void {
  const form = new FormData(formElement);
  const next = { ...state.draftForm };
  for (const key of Object.keys(next)) {
    if (key.endsWith("_enabled") || key.endsWith("_baseline") || key.endsWith("_primary")) {
      next[key] = "";
    }
  }
  for (const [key, value] of form.entries()) {
    if (typeof value === "string") {
      next[key] = value;
    }
  }
  state.draftForm = next;
}

function draftFromForm(formElement: HTMLFormElement): Json {
  const form = new FormData(formElement);
  const casesCount = optionalNumber(form, "cases_count");
  const maxConcurrency = optionalNumber(form, "max_concurrency");
  const timeoutMs = optionalNumber(form, "timeout_ms");
  const draft: Json = {
    experiment: {
      id: requiredFormString(form, "experiment_id", "Experiment ID"),
      name: requiredFormString(form, "experiment_name", "Experiment name"),
      description: String(form.get("description") ?? "").trim(),
    },
    runtime: {
      compute: { backend: String(form.get("compute_backend") ?? "local-docker") },
      storage: { backend: "local-fs" },
    },
    matrix: {
      variants: variantsFromForm(form),
      cases: {
        source: "file",
        path: requiredFormString(form, "cases_path", "Cases path"),
        ...(casesCount === null ? {} : { count: casesCount }),
      },
      repeats: optionalNumber(form, "repeats") ?? 1,
      seeds: parseNumberList(String(form.get("seeds") ?? "")),
    },
    metrics: metricsFromForm(form),
    scheduling: {
      comparison: "paired",
      shuffle_tasks: true,
      ...(maxConcurrency === null ? {} : { max_concurrency: maxConcurrency }),
    },
    stages: {
      build: {},
      run: {},
      analyze: {},
    },
    policy: {
      ...(timeoutMs === null ? {} : { timeout_ms: timeoutMs }),
      sanitization_profile: "hermetic_functional",
    },
  };
  return draft;
}

function draftResourcesFromForm(form: FormData): DraftResource[] {
  return [
    ...variantResourcesFromForm(form),
    ...metricResourcesFromForm(form),
  ];
}

function variantResourcesFromForm(form: FormData): DraftResource[] {
  const resources: DraftResource[] = [];
  for (let index = 0; index < 3; index += 1) {
    if (form.get(`variant_${index}_enabled`) !== "on") {
      continue;
    }
    const source = String(form.get(`variant_${index}_source`) ?? "inline");
    const displayName = String(form.get(`variant_${index}_name`) ?? "").trim() || `Variant ${index + 1}`;
    if (source === "alias") {
      resources.push({
        kind: "variant",
        label: displayName,
        source: "alias",
        alias: String(form.get(`variant_${index}_alias`) ?? "").trim(),
      });
      continue;
    }
    const id = String(form.get(`variant_${index}_id`) ?? "").trim();
    resources.push({
      kind: "variant",
      label: displayName,
      source: "inline",
      alias: id,
      object: {
        id,
        display_name: displayName,
        ...(form.get(`variant_${index}_baseline`) === "on" ? { baseline: true } : {}),
        config: parseKeyValueList(String(form.get(`variant_${index}_config`) ?? "")),
      },
    });
  }
  return resources.filter((resource) => resource.source === "alias" ? Boolean(resource.alias) : isJson(resource.object));
}

function metricResourcesFromForm(form: FormData): DraftResource[] {
  const resources: DraftResource[] = [];
  for (let index = 0; index < 2; index += 1) {
    if (form.get(`metric_${index}_enabled`) !== "on") {
      continue;
    }
    const displayName = String(form.get(`metric_${index}_name`) ?? "").trim() || `Metric ${index + 1}`;
    const id = String(form.get(`metric_${index}_id`) ?? "").trim();
    resources.push({
      kind: "metric",
      label: displayName,
      source: "inline",
      alias: id,
      object: {
        id,
        display_name: displayName,
        direction: String(form.get(`metric_${index}_direction`) ?? "maximize"),
        ...(form.get(`metric_${index}_primary`) === "on" ? { primary: true } : {}),
      },
    });
  }
  return resources;
}

function defaultDraftForm(): Record<string, string> {
  return {
    experiment_name: "Cookbook A/B Test",
    experiment_id: "cookbook_ab_test",
    compute_backend: "local-docker",
    description: "Baseline versus treatment over paired cases.",
    cases_path: "cases.jsonl",
    cases_count: "",
    repeats: "2",
    max_concurrency: "2",
    seeds: "11, 12",
    timeout_ms: "120000",
    variant_0_enabled: "on",
    variant_0_source: "inline",
    variant_0_name: "Baseline",
    variant_0_id: "baseline",
    variant_0_alias: "",
    variant_0_config: "model=gpt-5, mode=baseline",
    variant_0_baseline: "on",
    variant_1_enabled: "on",
    variant_1_source: "inline",
    variant_1_name: "Treatment",
    variant_1_id: "treatment",
    variant_1_alias: "",
    variant_1_config: "model=gpt-5, mode=treatment",
    variant_1_baseline: "",
    variant_2_enabled: "",
    variant_2_source: "inline",
    variant_2_name: "",
    variant_2_id: "",
    variant_2_alias: "",
    variant_2_config: "",
    variant_2_baseline: "",
    metric_0_enabled: "on",
    metric_0_name: "Resolved",
    metric_0_id: "resolved",
    metric_0_direction: "maximize",
    metric_0_primary: "on",
    metric_1_enabled: "",
    metric_1_name: "Keyword hits",
    metric_1_id: "keyword_hits",
    metric_1_direction: "maximize",
    metric_1_primary: "",
  };
}

function variantsFromForm(form: FormData): Json[] {
  const variants: Json[] = [];
  for (let index = 0; index < 3; index += 1) {
    if (form.get(`variant_${index}_enabled`) !== "on") {
      continue;
    }
    const source = String(form.get(`variant_${index}_source`) ?? "inline");
    const displayName = String(form.get(`variant_${index}_name`) ?? "").trim();
    if (source === "alias") {
      const alias = String(form.get(`variant_${index}_alias`) ?? "").trim();
      if (!alias) {
        throw new Error(`Variant ${index + 1} needs a saved handle.`);
      }
      variants.push({
        display_name: displayName || alias,
        registry: {
          alias,
          schema_version: "variant_v1",
        },
        ...(form.get(`variant_${index}_baseline`) === "on" ? { baseline: true } : {}),
      });
      continue;
    }
    const id = String(form.get(`variant_${index}_id`) ?? "").trim();
    if (!displayName || !id) {
      throw new Error(`Variant ${index + 1} needs a name and ID.`);
    }
    variants.push({
      id,
      display_name: displayName,
      ...(form.get(`variant_${index}_baseline`) === "on" ? { baseline: true } : {}),
      config: parseKeyValueList(String(form.get(`variant_${index}_config`) ?? "")),
    });
  }
  if (variants.length === 0) {
    throw new Error("Add at least one variant.");
  }
  return variants;
}

function metricsFromForm(form: FormData): Json[] {
  const metrics: Json[] = [];
  for (let index = 0; index < 2; index += 1) {
    if (form.get(`metric_${index}_enabled`) !== "on") {
      continue;
    }
    const displayName = String(form.get(`metric_${index}_name`) ?? "").trim();
    const id = String(form.get(`metric_${index}_id`) ?? "").trim();
    if (!displayName || !id) {
      throw new Error(`Metric ${index + 1} needs a name and ID.`);
    }
    metrics.push({
      id,
      display_name: displayName,
      direction: String(form.get(`metric_${index}_direction`) ?? "maximize"),
      ...(form.get(`metric_${index}_primary`) === "on" ? { primary: true } : {}),
    });
  }
  if (metrics.length === 0) {
    throw new Error("Add at least one metric.");
  }
  return metrics;
}

function parseKeyValueList(raw: string): Json {
  const result: Json = {};
  for (const chunk of raw.split(",")) {
    const [rawKey, ...rest] = chunk.split("=");
    const key = rawKey?.trim();
    if (!key) {
      continue;
    }
    result[key] = rest.join("=").trim();
  }
  return result;
}

function parseNumberList(raw: string): number[] {
  const values = raw.split(",")
    .map((value) => Number.parseInt(value.trim(), 10))
    .filter((value) => Number.isFinite(value));
  return values.length > 0 ? values : [1];
}

function requiredFormString(form: FormData, name: string, label: string): string {
  const value = String(form.get(name) ?? "").trim();
  if (!value) {
    throw new Error(`${label} is required.`);
  }
  return value;
}

function optionalNumber(form: FormData, name: string): number | null {
  const raw = String(form.get(name) ?? "").trim();
  if (!raw) {
    return null;
  }
  const value = Number.parseInt(raw, 10);
  return Number.isFinite(value) ? value : null;
}

function numberFromForm(form: FormData, name: string): number {
  return Number.parseInt(String(form.get(name) ?? "0"), 10);
}

function countStatus(rows: Json[], status: string): number {
  return rows.filter((row) => row.status === status).length;
}

function countQueuedRuns(): number {
  return state.runs.filter((run) => ["created", "waiting_for_runner"].includes(String(run.status))).length;
}

function countActiveRuns(): number {
  return state.runs.filter((run) => ["created", "waiting_for_runner", "running"].includes(String(run.status))).length;
}

function liveRunnerCount(): number {
  return state.instances.filter((instance) => instance.status === "online" && !isStaleHeartbeat(instance.last_heartbeat_at)).length;
}

function managedPool(): Json | null {
  if (state.runnerPoolId) {
    return state.pools.find((pool) => pool.runner_pool_id === state.runnerPoolId) ?? null;
  }
  return state.pools.find((pool) => pool.status === "active") ?? state.pools[0] ?? null;
}

function scopedPools(rows: Json[]): Json[] {
  if (!state.runnerPoolId) {
    return rows.filter((pool) => pool.status === "active");
  }
  return rows.filter((pool) => pool.runner_pool_id === state.runnerPoolId);
}

const ENTITY_KINDS = [
  "variant",
  "metric",
  "case",
  "dataset",
  "agent_app",
  "grader",
  "runtime_profile",
  "task_boundary",
  "trial_contract",
  "experiment_package",
];

function kindSelect(name: string, selected = "variant"): string {
  return `<select name="${escapeAttr(name)}">${ENTITY_KINDS.map((kind) => `<option value="${escapeAttr(kind)}" ${kind === selected ? "selected" : ""}>${escapeHtml(labelize(kind))}</option>`).join("")}</select>`;
}

function draftRaw(name: string): string {
  return state.draftForm[name] ?? "";
}

function draftValue(name: string, fallback = ""): string {
  return escapeAttr(state.draftForm[name] ?? fallback);
}

function draftChecked(name: string, fallback = ""): string {
  return (state.draftForm[name] ?? fallback) === "on" ? "checked" : "";
}

function option(value: string, selected: string, label = value): string {
  return `<option value="${escapeAttr(value)}" ${value === selected ? "selected" : ""}>${escapeHtml(label === value ? labelize(label) : label)}</option>`;
}

function defaultRegistryObjectJson(): string {
  return JSON.stringify({
    id: "baseline",
    display_name: "Baseline",
    config: {
      model: "gpt-5",
    },
  }, null, 2);
}

function packageTitle(pkg: Json): string {
  const manifest = isJson(pkg.manifest_json) ? pkg.manifest_json : {};
  const resolved = isJson(pkg.resolved_experiment_json)
    ? pkg.resolved_experiment_json
    : isJson(manifest.resolved_experiment)
      ? manifest.resolved_experiment
      : {};
  const experiment = isJson(resolved.experiment) ? resolved.experiment : {};
  return stringValue(experiment.name)
    ?? stringValue(experiment.id)
    ?? stringValue(manifest.name)
    ?? stringValue(manifest.id)
    ?? shortDigest(String(pkg.package_digest ?? ""));
}

function entityTitle(title: string, id: string): string {
  return `<div class="entity-title">${escapeHtml(title || shortDigest(id))}</div><div class="mono subdued">${escapeHtml(shortDigest(id))}</div>`;
}

function resourceLink(title: string, digest: string): string {
  return `<button type="button" class="link-button" data-resource-digest="${escapeAttr(digest)}">
    ${entityTitle(title, digest)}
  </button>`;
}

function resourceDisplayName(resource: Json): string {
  const canonical = isJson(resource.canonical_json) ? resource.canonical_json : {};
  return stringValue(canonical.display_name)
    ?? stringValue(canonical.name)
    ?? stringValue(canonical.id)
    ?? shortDigest(String(resource.content_digest ?? ""));
}

function shortDigest(value: string): string {
  if (!value) {
    return "";
  }
  if (value.length <= 24) {
    return value;
  }
  return `${value.slice(0, 16)}...${value.slice(-8)}`;
}

function stringValue(value: unknown): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

function titleForView(view: AppState["activeView"]): string {
  return {
    overview: "Operations Overview",
    packages: "Experiment Builds",
    authoring: state.authoringMode === "experiment" ? "Design Experiment" : "Resource Library",
    resource: state.selectedResource ? resourceDisplayName(state.selectedResource) : "Saved Resource",
    runs: "Run Experiment",
    runners: "Execution",
  }[view];
}

function subtitleForView(view: AppState["activeView"]): string {
  return {
    overview: "Control-plane health, queue pressure, and capacity at a glance.",
    packages: "Upload accepted build artifacts and inspect what is ready to run.",
    authoring: state.authoringMode === "experiment"
      ? "Compose a YAML experiment from readable fields, check it, and generate YAML when it is ready."
      : "Find, review, and explicitly save reusable experiment resources.",
    resource: "Saved resource identity, handles, and content.",
    runs: "Start experiments from accepted builds and watch their status.",
    runners: "Inspect the execution service, worker machines, and startup lifecycle.",
  }[view];
}

function requirementsCell(req: Json | undefined): string {
  if (!req) {
    return "";
  }
  return [
    pill(String(req.executor ?? ""), "info"),
    pill(String(req.arch ?? ""), ""),
    pill(`${String(req.cpu_count ?? "?")} cpu`, ""),
    pill(`${String(req.memory_mb ?? "?")} MB`, ""),
  ].join(" ");
}

function capabilitiesCell(cap: Json | undefined): string {
  if (!cap) {
    return "";
  }
  const executors = Array.isArray(cap.executors) ? cap.executors : [];
  const resources = Array.isArray(cap.resources) ? cap.resources : [];
  return `${list(executors.map(String))}<div class="label">${escapeHtml(resources.map(String).join(", "))}</div>`;
}

function list(items: string[]): string {
  if (items.length === 0) {
    return "";
  }
  return items.map((item) => pill(item, "")).join(" ");
}

function pill(text: string, tone: string): string {
  return `<span class="pill ${tone}">${escapeHtml(labelize(text || "none"))}</span>`;
}

function mono(text: string): string {
  return `<span class="mono">${escapeHtml(text)}</span>`;
}

function dateCell(value: unknown): string {
  if (typeof value !== "string" || !value) {
    return "";
  }
  return escapeHtml(new Date(value).toLocaleString());
}

function runnerStatusPill(instance: Json): string {
  const status = String(instance.status ?? "");
  if (status === "online" && isStaleHeartbeat(instance.last_heartbeat_at)) {
    return pill("stale", "warn");
  }
  return pill(status, toneForStatus(status));
}

function isStaleHeartbeat(value: unknown): boolean {
  if (typeof value !== "string" || !value) {
    return true;
  }
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) {
    return true;
  }
  return Date.now() - timestamp > 90_000;
}

function heartbeatAge(value: unknown): string {
  if (typeof value !== "string" || !value) {
    return "never";
  }
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) {
    return "unknown age";
  }
  const seconds = Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
  if (seconds < 60) {
    return `${seconds}s ago`;
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes}m ago`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 48) {
    return `${hours}h ago`;
  }
  return `${Math.floor(hours / 24)}d ago`;
}

function toneForStatus(status: string): string {
  if (["accepted", "active", "completed", "online"].includes(status)) {
    return "ok";
  }
  if (["created", "waiting_for_runner", "requested", "provisioning", "running"].includes(status)) {
    return "info";
  }
  if (["draining", "expired"].includes(status)) {
    return "warn";
  }
  if (["failed", "rejected", "offline", "unhealthy"].includes(status)) {
    return "bad";
  }
  if (["reaped"].includes(status)) {
    return "violet";
  }
  return "";
}

function toneForAlias(status: string): string {
  if (["available", "already_points_here"].includes(status)) {
    return "ok";
  }
  if (status === "conflicts") {
    return "bad";
  }
  return "";
}

function toneForIssue(severity: string): string {
  if (severity === "error") {
    return "bad";
  }
  if (severity === "warning") {
    return "warn";
  }
  return "info";
}

function toneForResourceReview(status: string): string {
  if (status === "reused") {
    return "ok";
  }
  if (status === "new" || status === "similar") {
    return "info";
  }
  if (status === "missing") {
    return "warn";
  }
  if (status === "conflict") {
    return "bad";
  }
  return "";
}

function statusLabel(status: string): string {
  return {
    reused: "saved",
    new: "new",
    similar: "similar",
    missing: "missing",
    conflict: "conflict",
  }[status] ?? status;
}

function handleStatusLabel(status: string): string {
  return {
    available: "available",
    already_points_here: "already saved",
    conflicts: "conflict",
  }[status] ?? status;
}

function actionLabel(action: string): string {
  return {
    register_object: "save resource",
    create_alias: "save handle",
    use_existing: "reuse saved resource",
  }[action] ?? action;
}

function labelize(value: string): string {
  const spaced = value.replaceAll("_", " ").replaceAll("-", " ");
  return spaced.replace(/\b[a-z]/g, (letter) => letter.toUpperCase());
}

function previewMessage(preview: Json): string {
  if (preview.total_slots === null || preview.total_slots === undefined) {
    return "Preview ready. Cases will be counted during build.";
  }
  return `Preview ready: ${String(preview.total_slots)} trial slots.`;
}

function arrayFrom(value: unknown): Json[] {
  return Array.isArray(value) ? value.filter((item): item is Json => !!item && typeof item === "object" && !Array.isArray(item)) : [];
}

function isJson(value: unknown): value is Json {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function messageForError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function escapeAttr(value: string): string {
  return escapeHtml(value);
}
