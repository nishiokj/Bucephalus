# Static Site Plan

The user docs are plain Markdown first. The repo now includes a minimal `mkdocs.yml` that mounts this user-facing path as a static site.

Preview locally:

```bash
pip install mkdocs-material
mkdocs serve
```

Build static files:

```bash
mkdocs build
```

Recommended navigation:

```yaml
nav:
  - Start Here:
      - User Docs: docs/user/index.md
      - Quickstart: docs/user/quickstart.md
      - What You Must Provide: docs/user/what-you-provide.md
  - Concepts:
      - Agent Runtime Contract: docs/user/agent-runtime-contract.md
      - Task Rows And Benchmarks: docs/user/task-rows.md
      - Graders And Mappers: docs/user/graders-and-mappers.md
      - Environment And Secrets: docs/user/env-and-secrets.md
  - Operations:
      - Inspecting Results: docs/user/inspecting-results.md
      - Troubleshooting: docs/user/troubleshooting.md
```

Good candidates:

- MkDocs Material: simplest Python-native path for this repo.
- VitePress: good if the repo moves documentation toward TypeScript tooling.
- Docusaurus: good if versioned/product docs become important.

Keep patch specs, audits, and architecture drafts under an internal/RFC section so they do not compete with the first-run path.
