#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import yaml

CASE_SOURCE = {
    "pg_001": "castor_canonical",
    "pg_002": "customer_of_customer",
    "pg_003": "regulatory_cascade",
    "pg_004": "brand_exposure_tweet",
    "pg_005": "noise_only_day",
    "pg_006": "near_miss_material",
    "pg_007": "unrelated_industry_earnings",
    "pg_008": "out_of_scope_regulation",
    "pg_009": "gpu_capacity_reallocation",
    "pg_010": "antimony_export_control",
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


def bounded_limit(value: Any, default: int = 8, maximum: int = 12) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return default
    if parsed < 1:
        return 1
    return min(parsed, maximum)


def normalized_query(value: Any) -> str:
    query = str(value or "").strip()
    if len(query) < 3:
        raise ValueError("search query must contain at least 3 non-whitespace characters")
    return query




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
        for lane in self.raw.get("procurement_lanes.yaml", {}).get("procurement_lanes", []) or []:
            lane_id = lane.get("id")
            commodity_id = lane.get("commodity_id")
            if lane_id == entity_id and commodity_id:
                edges.append({"from": lane_id, "to": commodity_id, "type": "lane_for_commodity", "origin_country": lane.get("origin_country")})
            if commodity_id == entity_id and lane_id:
                edges.append({"from": entity_id, "to": lane_id, "type": "commodity_has_procurement_lane", "origin_country": lane.get("origin_country"), "allocation_status": lane.get("allocation_status")})
        for tender in self.raw.get("customer_tenders.yaml", {}).get("customer_tenders", []) or []:
            tender_id = tender.get("id")
            commodity_id = tender.get("required_commodity_id")
            if tender_id == entity_id and commodity_id:
                edges.append({"from": tender_id, "to": commodity_id, "type": "program_requires_commodity", "customer": tender.get("customer_name")})
            if commodity_id == entity_id and tender_id:
                edges.append({"from": entity_id, "to": tender_id, "type": "commodity_required_by_program", "customer": tender.get("customer_name")})
        for item in self.raw.get("inventory.yaml", {}).get("commodity_inventory", []) or []:
            inventory_id = item.get("id")
            commodity_id = item.get("commodity_id")
            if inventory_id == entity_id and commodity_id:
                edges.append({"from": inventory_id, "to": commodity_id, "type": "inventory_for_commodity"})
            if commodity_id == entity_id and inventory_id:
                edges.append({"from": entity_id, "to": inventory_id, "type": "commodity_has_inventory"})
        return {"entity_id": entity_id, "edges": edges}

    def trace_procurement_exposure(self, entity_ids: list[str]) -> dict[str, Any]:
        """Return neutral procurement facts for the reachable commodities. No verdict:
        whether reserving a lane is warranted is left for the agent to compose from
        these numbers and the event."""
        targets = set(entity_ids)
        lanes = self.raw.get("procurement_lanes.yaml", {}).get("procurement_lanes", []) or []
        tenders = self.raw.get("customer_tenders.yaml", {}).get("customer_tenders", []) or []
        inventory = self.raw.get("inventory.yaml", {}).get("commodity_inventory", []) or []
        commodity_targets = {target for target in targets if any(lane.get("commodity_id") == target for lane in lanes)}
        for lane in lanes:
            if lane.get("id") in targets and lane.get("commodity_id"):
                commodity_targets.add(lane["commodity_id"])
        for tender in tenders:
            if tender.get("id") in targets and tender.get("required_commodity_id"):
                commodity_targets.add(tender["required_commodity_id"])

        def grade_match(lane: dict[str, Any], tender: dict[str, Any]) -> bool:
            required = str(tender.get("required_grade") or "")
            certifications = [str(item) for item in lane.get("certifications", []) or []]
            return not required or any(required == cert or required.startswith(cert) for cert in certifications)

        def free_inventory(commodity_id: str) -> int:
            return sum(
                int(item.get("quantity_available_tons", 0)) - int(item.get("committed_tons", 0))
                for item in inventory if item.get("commodity_id") == commodity_id
            )

        tender_facts: list[dict[str, Any]] = []
        for tender in tenders:
            commodity_id = tender.get("required_commodity_id")
            if commodity_id not in commodity_targets:
                continue
            lane_facts = [
                {
                    "id": lane.get("id"),
                    "origin_country": lane.get("origin_country"),
                    "qualified": lane.get("qualified"),
                    "allocation_status": lane.get("allocation_status"),
                    "available_q3_tons": lane.get("available_q3_tons"),
                    "committed_q3_tons": lane.get("committed_q3_tons"),
                    "lead_time_days": lane.get("lead_time_days"),
                    "price_usd_per_ton": lane.get("price_usd_per_ton"),
                    "crop_stage": lane.get("crop_stage"),
                    "harvest_window": lane.get("harvest_window"),
                    "grade_match": grade_match(lane, tender),
                }
                for lane in lanes if lane.get("commodity_id") == commodity_id
            ]
            tender_facts.append({
                "id": tender.get("id"),
                "customer_name": tender.get("customer_name"),
                "required_commodity_id": commodity_id,
                "required_grade": tender.get("required_grade"),
                "required_q3_tons": tender.get("required_q3_tons"),
                "contracting_window": tender.get("contracting_window"),
                "revenue_opportunity_usd": tender.get("revenue_opportunity_usd"),
                "margin_usd": tender.get("margin_usd"),
                "penalty_if_missed_usd": tender.get("penalty_if_missed_usd"),
                "price_pass_through": tender.get("price_pass_through"),
                "commodity_free_inventory_tons": free_inventory(commodity_id),
                "lanes": lane_facts,
            })
        return {
            "queried_entity_ids": sorted(targets),
            "commodities": sorted(commodity_targets),
            "commodity_free_inventory_tons": {c: free_inventory(c) for c in sorted(commodity_targets)},
            "tenders": tender_facts,
        }

    def trace_accelerator_exposure(self, entity_ids: list[str]) -> dict[str, Any]:
        """Return neutral capacity facts for the reachable programs. No verdict and no
        do-not-prioritize hints: requested units, build status, input lead times, and
        transferable capacity are reported for the agent to compose with the event."""
        targets = set(entity_ids)
        programs = self.raw.get("accelerator_programs.yaml", {}).get("accelerator_programs", []) or []
        allocations = self.raw.get("supply_allocations.yaml", {}).get("supply_allocations", []) or []
        commitments = self.raw.get("customer_commitments.yaml", {}).get("customer_commitments", []) or []
        wip = self.raw.get("manufacturing_wip.yaml", {}).get("manufacturing_wip", []) or []
        inventory = self.raw.get("inventory.yaml", {}).get("inventory", []) or []

        program_targets = {target for target in targets if any(program.get("id") == target for program in programs)}
        for target in targets:
            lowered = target.lower()
            for program in programs:
                if lowered in scalar_text(program).lower():
                    program_targets.add(program["id"])
        for allocation in allocations:
            if allocation.get("input_type") in targets:
                for program_id in allocation.get("compatible_programs", []) or []:
                    program_targets.add(program_id)

        program_facts: list[dict[str, Any]] = []
        for program in programs:
            if program.get("id") not in program_targets:
                continue
            pid = program["id"]
            commits = [c for c in commitments if c.get("program_id") == pid]
            builds = [w for w in wip if w.get("program_id") == pid]
            inv = [i for i in inventory if i.get("program_id") == pid]
            program_facts.append({
                "program_id": pid,
                "product_family": program.get("product_family"),
                "customer_id": program.get("customer_id"),
                "memory_config": program.get("memory_config"),
                "commitments": [
                    {
                        "id": c.get("id"),
                        "customer_name": c.get("customer_name"),
                        "requested_units": c.get("requested_units"),
                        "base_ship_window": c.get("base_ship_window"),
                        "deployment_flexibility": c.get("deployment_flexibility"),
                        "revenue_usd": c.get("revenue_usd"),
                        "penalty_if_late_usd": c.get("penalty_if_late_usd"),
                        "approval_required_for_reallocation": c.get("approval_required_for_reallocation"),
                    }
                    for c in commits
                ],
                "work_in_progress": [
                    {"id": w.get("id"), "units_in_build": w.get("units_in_build"), "status": w.get("status")}
                    for w in builds
                ],
                "finished_units": sum(int(i.get("finished_units", 0)) for i in inv),
                "reserved_units": sum(int(i.get("reserved_units", 0)) for i in inv),
            })
        input_allocations = [
            {
                "id": allocation.get("id"),
                "input_type": allocation.get("input_type"),
                "compatible_programs": allocation.get("compatible_programs"),
                "allocated_units": allocation.get("allocated_units"),
                "available_transfer_units": allocation.get("available_transfer_units"),
                "lead_time_days": allocation.get("lead_time_days"),
            }
            for allocation in allocations
            if set(allocation.get("compatible_programs", []) or []) & program_targets
        ]
        return {
            "queried_entity_ids": sorted(targets),
            "affected_programs": sorted(program_targets),
            "programs": program_facts,
            "input_allocations": input_allocations,
        }

    def trace_exposure(self, entity_ids: list[str]) -> dict[str, Any]:
        if "accelerator_programs.yaml" in self.raw:
            return self.trace_accelerator_exposure(entity_ids)
        if "customer_tenders.yaml" in self.raw:
            return self.trace_procurement_exposure(entity_ids)
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
        query = normalized_query(request.get("query"))
        return case_store.search(query, bounded_limit(request.get("limit")))
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
