FROM nova-service:amd64-pg024-traceids

USER root

RUN apt-get update \
  && apt-get install -y --no-install-recommends python3 python3-pip python3-yaml ca-certificates \
  && python3 -m pip install --break-system-packages "mcp[cli]==1.27.2" \
  && rm -rf /var/lib/apt/lists/*

# Cloud demo runtime disables Nova GraphD so the run only needs the compact
# Codex OAuth credential. The large local graphd.db is not a Secret Manager
# payload and is not needed for the benchmark task.
COPY agent/pg_mcp_server.py agent/mcp.json /opt/peter-gregory/agent/
COPY agent/nova-config.cloud.json /opt/peter-gregory/agent/nova-config.json

RUN chmod +x /opt/peter-gregory/agent/pg_mcp_server.py \
  && python3 -c "import mcp; import importlib.util; spec = importlib.util.spec_from_file_location('pg_mcp_server', '/opt/peter-gregory/agent/pg_mcp_server.py'); module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)"

WORKDIR /bucephalus/workspace
