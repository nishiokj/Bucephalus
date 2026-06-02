#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.request

def post(payload: dict) -> int:
    url = os.environ.get("PG_DATA_API_URL", "http://pg-data-api:9757")
    case_id = os.environ.get("PG_CASE_ID")
    if not case_id:
        print(json.dumps({"ok": False, "error": "PG_CASE_ID not set"}, indent=2), file=sys.stderr)
        return 1
    payload = {"case_id": case_id, **payload}
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"}, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            sys.stdout.write(resp.read().decode("utf-8"))
            sys.stdout.write("\n")
            return 0
    except Exception as exc:
        print(json.dumps({"ok": False, "error": str(exc)}, indent=2), file=sys.stderr)
        return 1


def main() -> int:
    parser = argparse.ArgumentParser(description="Query the Peter Gregory read-only company data API.")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("overview", help="Show available collections, counts, and product families.")
    search = sub.add_parser("search", help="Substring search over indexed entity records.")
    search.add_argument("query")
    search.add_argument("--limit", type=int, default=8)
    entity = sub.add_parser("get_entity", help="Return indexed records for one entity id.")
    entity.add_argument("entity_id")
    neighbors = sub.add_parser("neighbors", help="Return graph neighbors for one entity id.")
    neighbors.add_argument("entity_id")
    trace = sub.add_parser("trace_exposure", help="Trace one or more entity ids downstream to products/orders/revenue.")
    trace.add_argument("entity_id", nargs="+")
    orders = sub.add_parser("orders_for_product", help="Return open orders for a product id.")
    orders.add_argument("product_id")

    args = parser.parse_args()
    if args.command == "overview":
        return post({"command": "overview"})
    if args.command == "search":
        return post({"command": "search", "query": args.query, "limit": args.limit})
    if args.command == "get_entity":
        return post({"command": "get_entity", "entity_id": args.entity_id})
    if args.command == "neighbors":
        return post({"command": "neighbors", "entity_id": args.entity_id})
    if args.command == "trace_exposure":
        return post({"command": "trace_exposure", "entity_ids": args.entity_id})
    if args.command == "orders_for_product":
        return post({"command": "orders_for_product", "product_id": args.product_id})
    raise AssertionError(args.command)


if __name__ == "__main__":
    raise SystemExit(main())
