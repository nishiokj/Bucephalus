#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import urllib.request
from typing import Any, Literal

from mcp.server.fastmcp import FastMCP


PG_DATA_API_URL = os.environ.get("PG_DATA_API_URL", "http://pg-data-api:9757")
CaseId = Literal["pg_001", "pg_002", "pg_003", "pg_004", "pg_005", "pg_006", "pg_007", "pg_008", "pg_009"]

mcp = FastMCP("Peter Gregory company data")


def post(case_id: CaseId, payload: dict[str, Any]) -> dict[str, Any]:
    request_payload = {"case_id": case_id, **payload}
    data = json.dumps(request_payload).encode("utf-8")
    req = urllib.request.Request(
        PG_DATA_API_URL,
        data=data,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=20) as response:
        decoded = json.loads(response.read().decode("utf-8"))
    if not isinstance(decoded, dict):
        raise ValueError("Peter Gregory data API returned a non-object response")
    if not decoded.get("ok"):
        raise ValueError(str(decoded.get("error") or "Peter Gregory data API request failed"))
    result = decoded.get("result")
    if not isinstance(result, dict):
        raise ValueError("Peter Gregory data API result was not an object")
    return result


@mcp.tool()
def pg_overview(case_id: CaseId) -> dict[str, Any]:
    """Show available Peter Gregory record collections, product families, and query commands for one case."""
    return post(case_id, {"command": "overview"})


@mcp.tool()
def pg_search(case_id: CaseId, query: str, limit: int = 8) -> dict[str, Any]:
    """Search indexed Peter Gregory entity records for one case."""
    return post(case_id, {"command": "search", "query": query, "limit": limit})



@mcp.tool()
def pg_get_entity(case_id: CaseId, entity_id: str) -> dict[str, Any]:
    """Return records for one Peter Gregory entity id in one case."""
    return post(case_id, {"command": "get_entity", "entity_id": entity_id})


@mcp.tool()
def pg_neighbors(case_id: CaseId, entity_id: str) -> dict[str, Any]:
    """Return graph neighbors for one Peter Gregory entity id in one case."""
    return post(case_id, {"command": "neighbors", "entity_id": entity_id})


@mcp.tool()
def pg_trace_exposure(case_id: CaseId, entity_ids: list[str]) -> dict[str, Any]:
    """Trace one or more Peter Gregory entity ids downstream to affected products, orders, revenue, and inventory in one case."""
    return post(case_id, {"command": "trace_exposure", "entity_ids": entity_ids})


@mcp.tool()
def pg_orders_for_product(case_id: CaseId, product_id: str) -> dict[str, Any]:
    """Return open Peter Gregory orders for one product id in one case."""
    return post(case_id, {"command": "orders_for_product", "product_id": product_id})


if __name__ == "__main__":
    mcp.run()
