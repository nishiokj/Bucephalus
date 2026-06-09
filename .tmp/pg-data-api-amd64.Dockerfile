FROM peter-gregory-v2-state-only-nova-cloud:amd64-pg024

COPY runtime-data/workspaces /opt/peter-gregory-data/workspaces
COPY agent/pg_data_api.py /opt/peter-gregory/pg_data_api.py

EXPOSE 9757

CMD ["python3", "/opt/peter-gregory/pg_data_api.py", "--data-root", "/opt/peter-gregory-data/workspaces", "--host", "0.0.0.0", "--port", "9757"]
