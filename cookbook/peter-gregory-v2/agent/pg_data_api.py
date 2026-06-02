#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import yaml

CASE_SOURCE = {
    "pg_001": "sesame_canonical",
    "pg_002": "customer_of_customer",
    "pg_003": "regulatory_cascade",
    "pg_004": "brand_exposure_tweet",
    "pg_005": "noise_only_day",
    "pg_006": "near_miss_material",
    "pg_007": "unrelated_industry_earnings",
    "pg_008": "out_of_scope_regulation",
}


def load_yaml(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as handle:
        return yaml.safe_load(handle) or {}


def scalar_text(value: Any) -> str:
    if isinstance(value, (str, int, float, bool)) or value is None:
        return "" if value is None else str(value)
    if isinstance(value, list):
        return " ".join(scalar_text(item) for item in value)
    if isinstance(value, dict):
        return " ".join(scalar_text(item) for item in value.values())
    return str(value)


def excerpt(text: str, query: str, window: int = 180) -> str:
    lower = text.lower()
    needle = query.lower()
    idx = lower.find(needle)
    if idx < 0:
        return text[:window]
    start = max(0, idx - window // 2)
    end = min(len(text), idx + len(query) + window // 2)
    return text[start:end]


class RecordStore:
    def __init__(self, root: Path):
        self.root = root
        self.records = root / "records"
        self.raw: dict[str, Any] = {}
        for path in sorted(self.records.glob("*.yaml")):
            self.raw[path.name] = load_yaml(path)
        profile = self.records / "company-profile.md"
        self.company_profile = profile.read_text(encoding="utf-8") if profile.exists() else ""
        self.entities: dict[str, list[dict[str, Any]]] = {}
        self.index()

    def index(self) -> None:
        for filename, doc in self.raw.items():
            if not isinstance(doc, dict):
                continue
            for collection, items in doc.items():
                if not isinstance(items, list):
                    continue
                for item in items:
                    if not isinstance(item, dict):
                        continue
                    entity_id = item.get("id") or item.get("product_id") or item.get("component_id")
                    if isinstance(entity_id, str):
                        self.entities.setdefault(entity_id, []).append({
                            "file": f"records/{filename}",
                            "collection": collection,
                            "record": item,
                        })
                    if filename == "bom.yaml":
                        product_id = item.get("product_id")
                        for component in item.get("components", []) or []:
                            component_id = component.get("component_id")
                            if isinstance(component_id, str):
                                self.entities.setdefault(component_id, []).append({
                                    "file": "records/bom.yaml",
                                    "collection": "bom_components",
                                    "product_id": product_id,
                                    "record": component,
                                })
                            for input_id in component.get("inputs", []) or []:
                                if isinstance(input_id, str):
                                    self.entities.setdefault(input_id, [])

    def overview(self) -> dict[str, Any]:
        counts: dict[str, int] = {}
        for doc in self.raw.values():
            if not isinstance(doc, dict):
                continue
            for collection, items in doc.items():
                if isinstance(items, list):
                    counts[collection] = len(items)
        products = self.raw.get("products.yaml", {}).get("products", [])
        families = sorted({item.get("family") for item in products if isinstance(item, dict) and item.get("family")})
        return {
            "record_collections": counts,
            "product_families": families,
            "query_commands": ["overview", "search", "get_entity", "neighbors", "trace_exposure", "orders_for_product"],
        }

    def search(self, query: str, limit: int = 8) -> dict[str, Any]:
        hits: list[dict[str, Any]] = []
        needle = query.lower()
        for filename, doc in self.raw.items():
            if not isinstance(doc, dict):
                continue
            for collection, items in doc.items():
                if not isinstance(items, list):
                    continue
                for item in items:
                    text = scalar_text(item)
                    if needle in text.lower():
                        hits.append({
                            "file": f"records/{filename}",
                            "collection": collection,
                            "id": item.get("id") or item.get("product_id") or item.get("component_id"),
                            "name": item.get("name") or item.get("customer"),
                            "excerpt": excerpt(text, query),
                        })
                        if len(hits) >= limit:
                            return {"query": query, "hits": hits}
        return {"query": query, "hits": hits}

    def get_entity(self, entity_id: str) -> dict[str, Any]:
        return {"entity_id": entity_id, "records": self.entities.get(entity_id, [])}

    def neighbors(self, entity_id: str) -> dict[str, Any]:
        edges: list[dict[str, Any]] = []
        for material in self.raw.get("materials.yaml", {}).get("materials", []) or []:
            if material.get("id") == entity_id:
                for source in material.get("sources", []) or []:
                    edges.append({"from": entity_id, "to": source, "type": "sourced_from"})
                for sub in material.get("substitutes", []) or []:
                    edges.append({"from": entity_id, "to": sub, "type": "substitute"})
            if entity_id in (material.get("sources", []) or []):
                edges.append({"from": entity_id, "to": material.get("id"), "type": "source_for_material"})
        for supplier in self.raw.get("suppliers.yaml", {}).get("suppliers", []) or []:
            for supplied in supplier.get("supplies", []) or []:
                supplied_id = supplied.get("input_id") or supplied.get("component_id")
                if supplied_id == entity_id:
                    edges.append({"from": supplier.get("id"), "to": entity_id, "type": "supplier_supplies", "supplier": supplier.get("name")})
                if supplier.get("id") == entity_id and supplied_id:
                    edges.append({"from": entity_id, "to": supplied_id, "type": "supplies"})
        for bom in self.raw.get("bom.yaml", {}).get("boms", []) or []:
            product_id = bom.get("product_id")
            if product_id == entity_id:
                for component in bom.get("components", []) or []:
                    if component.get("component_id"):
                        edges.append({"from": entity_id, "to": component.get("component_id"), "type": "product_has_component"})
            for component in bom.get("components", []) or []:
                component_id = component.get("component_id")
                if component_id == entity_id and product_id:
                    edges.append({"from": entity_id, "to": product_id, "type": "component_in_product"})
                for input_id in component.get("inputs", []) or []:
                    if input_id == entity_id and component_id:
                        edges.append({"from": entity_id, "to": component_id, "type": "input_to_component"})
                    if component_id == entity_id:
                        edges.append({"from": entity_id, "to": input_id, "type": "component_uses_input"})
        for order in self.raw.get("orders.yaml", {}).get("orders", []) or []:
            if order.get("product_id") == entity_id:
                edges.append({"from": entity_id, "to": order.get("id"), "type": "product_has_open_order", "customer": order.get("customer")})
            if order.get("id") == entity_id:
                edges.append({"from": entity_id, "to": order.get("product_id"), "type": "order_for_product"})
        return {"entity_id": entity_id, "edges": edges}

    def trace_exposure(self, entity_ids: list[str]) -> dict[str, Any]:
        targets = set(entity_ids)
        affected_products: set[str] = set()
        trace_edges: list[dict[str, Any]] = []
        for order in self.raw.get("orders.yaml", {}).get("orders", []) or []:
            if order.get("product_id") in targets:
                affected_products.add(order["product_id"])
        for bom in self.raw.get("bom.yaml", {}).get("boms", []) or []:
            product_id = bom.get("product_id")
            for component in bom.get("components", []) or []:
                component_id = component.get("component_id")
                if component_id in targets and product_id:
                    affected_products.add(product_id)
                    trace_edges.append({"from": component_id, "to": product_id, "type": "component_in_product"})
                for input_id in component.get("inputs", []) or []:
                    if input_id in targets and product_id:
                        affected_products.add(product_id)
                        if component_id:
                            trace_edges.append({"from": input_id, "to": component_id, "type": "input_to_component"})
                            trace_edges.append({"from": component_id, "to": product_id, "type": "component_in_product"})
        orders = [order for order in self.raw.get("orders.yaml", {}).get("orders", []) or [] if order.get("product_id") in affected_products]
        inventory = [item for item in self.raw.get("inventory.yaml", {}).get("inventory", []) or [] if item.get("product_id") in affected_products]
        revenue = sum(int(order.get("quantity", 0)) * int(order.get("unit_revenue", 0)) for order in orders)
        needed = sum(int(order.get("quantity", 0)) for order in orders)
        on_hand = sum(int(item.get("quantity_available", 0)) for item in inventory)
        return {
            "queried_entity_ids": sorted(targets),
            "affected_products": sorted(affected_products),
            "affected_orders": [order.get("id") for order in orders if order.get("id")],
            "affected_customers": sorted({order.get("customer") for order in orders if order.get("customer")}),
            "revenue_at_risk": revenue,
            "constrained_inventory": on_hand < needed,
            "inventory_on_hand": on_hand,
            "inventory_needed": needed,
            "trace_edges": trace_edges,
        }

    def orders_for_product(self, product_id: str) -> dict[str, Any]:
        orders = [order for order in self.raw.get("orders.yaml", {}).get("orders", []) or [] if order.get("product_id") == product_id]
        return {"product_id": product_id, "orders": orders}


class MultiCaseStore:
    def __init__(self, data_root: Path):
        self.stores = {
            case_id: RecordStore(data_root / scenario)
            for case_id, scenario in CASE_SOURCE.items()
        }

    def for_case(self, case_id: str) -> RecordStore:
        try:
            return self.stores[case_id]
        except KeyError as exc:
            raise ValueError("unknown or missing case_id") from exc


def handle(store: MultiCaseStore, request: dict[str, Any]) -> dict[str, Any]:
    case_store = store.for_case(str(request.get("case_id") or ""))
    command = request.get("command")
    if command == "overview":
        return case_store.overview()
    if command == "search":
        query = str(request.get("query") or "")
        if not query:
            raise ValueError("search requires query")
        return case_store.search(query, int(request.get("limit") or 8))
    if command == "get_entity":
        return case_store.get_entity(str(request.get("entity_id") or ""))
    if command == "neighbors":
        return case_store.neighbors(str(request.get("entity_id") or ""))
    if command == "trace_exposure":
        ids = request.get("entity_ids") or request.get("entity_id") or []
        if isinstance(ids, str):
            ids = [ids]
        return case_store.trace_exposure([str(item) for item in ids])
    if command == "orders_for_product":
        return case_store.orders_for_product(str(request.get("product_id") or ""))
    raise ValueError(f"unknown command: {command}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-root", required=True)
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, required=True)
    args = parser.parse_args()
    store = MultiCaseStore(Path(args.data_root))

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self) -> None:  # noqa: N802
            try:
                size = int(self.headers.get("Content-Length", "0"))
                request = json.loads(self.rfile.read(size).decode("utf-8"))
                response = {"ok": True, "result": handle(store, request)}
                status = 200
            except Exception as exc:
                response = {"ok": False, "error": str(exc)}
                status = 400
            data = json.dumps(response, indent=2, sort_keys=True).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, _format: str, *_args: Any) -> None:
            return

    ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
