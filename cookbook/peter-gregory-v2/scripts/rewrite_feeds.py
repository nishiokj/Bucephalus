#!/usr/bin/env python3
"""One-shot: rewrite event feeds for REGISTER-PARITY (Data Recipe E-group).

Each signal / near-miss event is preserved verbatim (keeps the causal trace + oracle
targets). Selected filler events are rewritten into specific-but-irrelevant events of
comparable-or-greater length — texture pointing to the WRONG commodity/region/industry,
chosen to never reach the company's real inputs (castor, silicon, antimony, fluoropolymer,
rosin / rail + transit accounts) or, for the food and accelerator companies, their real
programs. The noise-only day (pg_005) gets equally-serious events so "a serious event
exists => alert" no longer holds.

Edits the full-arm cases.jsonl in place (prompt + event_stream field); regenerate the
ablation arms afterward. Idempotent-ish: re-running with the same NEW streams is a no-op
only if the old text still matches, so run once against the pre-rewrite feeds.
"""
from __future__ import annotations
import json, pathlib

NEW = {}

NEW["pg_001"] = """# External Event Feed
# Date: 2026-06-15

## H01
Vietnamese robusta coffee exporters report a third straight month of below-average shipment volumes as port congestion at Ho Chi Minh City and an erratic monsoon trim deliverable Q3 lots, pushing London futures to a nine-month high.

## H02
A major smartphone assembler guides June-quarter unit shipments down about 6% on soft consumer demand in China and Western Europe, citing extended replacement cycles and elevated channel inventory of mid-tier handsets.

## H03
Brazil's Northeast is projected to see its worst January-March rainfall deficit in a decade, with Bahia agricultural agencies warning that rain-fed smallholder crops planted at the start of the wet season may see poor establishment.

## H04
An OEM supplier publishes a routine corporate-responsibility statement.

## H05
Red Sea diversions keep Asia-to-Europe container rates elevated for apparel and furniture importers, with carriers adding a fourth week of transit time around the Cape of Good Hope and blanking two sailings on the Far East-Mediterranean loop.

## H06
North American rail operators report normal intermodal terminal dwell times.

## H07
Industry conference attendance projections beat last year by modest margins.

## H08
A staffing firm reports stable wage inflation across light industrial roles.

## H09
A Canadian labor mediator schedules talks with port clerks next week.

## H10
A regional polymer compounder announces a routine ISO recertification.

## H11
A large US discount retailer beats quarterly comparable sales on grocery and household staples but warns that discretionary general-merchandise demand stayed soft through May, trimming its full-year operating-margin outlook.

## H12
Maritime fuel benchmarks closed near month-ago levels with low volatility.

## H13
A coastal flood watch is issued for the upper Texas Gulf Coast through the weekend, warning of heavy rain near industrial warehousing districts; operators note most resin and packaging stock sits above grade and no facility closures are planned.
"""

NEW["pg_002"] = """# Earnings & Analyst Feed
# Date: 2026-06-15

## E01 — Generic Issuer
A specialty REIT publishes a routine occupancy update with no surprises.

## E02 — Generic Issuer
An asset manager reports inflows into infrastructure equity funds.

## E03 — River Transport Desk
Mississippi River low-water restrictions are forcing bulk shippers to cut barge loads during peak export season. Logistics desks expect some time-sensitive freight to seek overland routes with spare line-haul capacity.

## E04 — Continental Air Group
A full-service airline trims full-year unit-revenue guidance, citing softening transatlantic leisure fares and higher regional-jet maintenance costs, while reaffirming its fleet-delivery schedule and summer capacity plan.

## E05 — Generic Issuer
A defense contractor reaffirms full-year backlog conversion guidance.

## E06 — Generic Issuer
A semiconductor equipment maker confirms maintenance capex unchanged.

## E07 — Generic Issuer
An auto-parts retailer posts comparable-store sales growth of 1.5%.

## E08 — Lumen Stream Media
A subscription streaming platform beats on net additions but guides margins lower, blaming content amortization and a stronger dollar on overseas ad sales; management reiterates a password-sharing crackdown timeline.

## E09 — Generic Issuer
A consumer-staples giant beats by a penny on routine volume growth.

## E10 — Generic Issuer
A regional bank affirms its quarterly outlook; loan-loss provisions trend flat.

## E11 — Cornerstone Residential
A national homebuilder reports orders up 4% as mortgage-rate buydowns support entry-level demand, but flags lumber and land-development cost inflation and a longer cycle time in two Sun Belt markets.
"""

NEW["pg_003"] = """# Regulatory & Standards Feed
# Date: 2026-06-15

## R01
A trade publication summarizes mostly unchanged Q2 rule-comment filings.

## R02
A regional water board updates its lab method for a non-related contaminant.

## R03
FCC dismisses a low-priority petition on amateur radio band sharing.

## R04
A federal infrastructure grant office updates its domestic-content attestation template for newly disbursed projects. The revision is administrative and applies only to prime-contractor reporting format, not supplier eligibility, material origin, or existing purchase orders.

## R05
A securities regulator finalizes expanded climate-disclosure rules requiring large filers to report Scope 1 and 2 emissions and material transition risks in annual filings, with phased assurance requirements beginning in fiscal 2027 for accelerated filers.

## R06
EPA finalizes a non-controversial update to combustion engine reporting.

## R07
The IRS publishes guidance on R&D capitalization that does not affect our segment.

## R08
EU customs publishes a detailed classification notice reassigning several non-related polymer grades between tariff headings, clarifying duty treatment for consumer packaging films and instructing importers to update binding tariff information references before the autumn quota window.

## R09
OSHA finalizes a heat-illness prevention rule for outdoor workers.

## R10
A banking supervisor opens consultation on revised capital treatment for operational risk, proposing a standardized business-indicator approach and higher output floors that would phase in over three years for internationally active lenders.

## R11
An obscure ISO subcommittee publishes a routine corrigendum to a packaging standard.
"""

NEW["pg_004"] = """# Social Chatter Feed
# Date: 2026-06-15

## T01  (190,000 engagements)
Aurora visible as far south as 38 degrees latitude tonight after a G4 geomagnetic storm; the last comparable Kp-8 event was October 2024, and grid operators say they are watching geomagnetically induced currents on high-latitude transformers.

## T02  (22,000 engagements)
Coffee is the only opinion I have today.

## T03  (4,300 engagements)
Reminder to drink water and stretch every hour.

## T04  (260,000 engagements)
A mid-size metro just broke a 221-day transit strike record as 14,000 operators returned to work this morning; ridership is still down roughly 12% year over year and the agency says full schedules resume next week.

## T05  (412,000 engagements)
Entomologists report that two distant periodical cicada populations, one on a 13-year cycle and one on a 17-year cycle, are emerging in the same season for the first time in 221 years. Regional agencies are monitoring localized agricultural nuisance reports, but no commodity impact has been confirmed.

## T06  (600 engagements)
The new firmware update fixed my smart bulb finally.

## T07  (3,100 engagements)
Conference WiFi is somehow worse this year.

## T08  (305,000 engagements)
Absolutely unreal finish: the marathon world record fell by 31 seconds in cool conditions, the third major record to drop this spring, and analysts are crediting the new carbon-plate super-shoe stack everyone is now arguing about online.

## T09  (2,700 engagements)
Stretching the deadline like a piece of taffy.

## T10  (150,000 engagements)
Tonight's rare planetary alignment puts six planets across the pre-dawn sky; the next time the inner four cluster this tightly is 2040, and stargazers are posting wide-angle shots from dark-sky parks.

## T11  (11,500 engagements)
Local farmer's market crushing it this season.
"""

NEW["pg_005"] = """# External Event Feed
# Date: 2026-06-15

## H01
An OEM supplier publishes a routine corporate-responsibility statement.

## H02
A Canadian labor mediator schedules talks with port clerks next week.

## H03
A staffing firm reports stable wage inflation across light industrial roles.

## H04
A coastal flood watch is issued for the upper Texas Gulf Coast through the weekend, warning of heavy rain near industrial warehousing districts; operators note most resin and packaging stock sits above grade and no facility closures are planned.

## H05
Analysts expect flat demand for municipal control systems through July.

## H06
North American rail operators report normal intermodal terminal dwell times.

## H07
A bond rating agency affirms an investment-grade outlook on a public transit issuer.

## H08
Red Sea diversions keep Asia-to-Europe container rates elevated for apparel and furniture importers, with carriers adding a fourth week of transit time around the Cape of Good Hope and blanking two sailings on the Far East-Mediterranean loop.

## H09
Vietnamese robusta coffee exporters report a third straight month of below-average shipment volumes as port congestion at Ho Chi Minh City and an erratic monsoon trim deliverable Q3 lots, pushing London futures to a nine-month high.

## H10
A regional polymer compounder announces a routine ISO recertification.

## H11
Maritime fuel benchmarks closed near month-ago levels with low volatility.

## H12
A major smartphone assembler guides June-quarter unit shipments down about 6% on soft consumer demand in China and Western Europe, citing extended replacement cycles and elevated channel inventory of mid-tier handsets.

## H13
Two regional utilities settle a longstanding interconnect rate dispute.

## H14
EU inspectors begin a paperwork campaign for selected medical-device labels.
"""

NEW["pg_006"] = """# External Event Feed
# Date: 2026-06-15

## H01
EU inspectors begin a paperwork campaign for selected medical-device labels.

## H02
A staffing firm reports stable wage inflation across light industrial roles.

## H03
Analysts expect flat demand for municipal control systems through July.

## H04
A specialty alumina producer says maintenance at one kiln finished early.

## H05
Cobalt prices surge to multi-year highs after a Glencore production notice; specialty battery cathode producers warn of margin pressure through Q3. Analysts flag tight neodymium and dysprosium as parallel rare-earth bottlenecks.

## H06
North American rail operators report normal intermodal terminal dwell times.

## H07
A regional polymer compounder announces a routine ISO recertification.

## H08
A large US discount retailer beats quarterly comparable sales on grocery and household staples but warns that discretionary general-merchandise demand stayed soft through May, trimming its full-year operating-margin outlook.

## H09
Vietnamese robusta coffee exporters report a third straight month of below-average shipment volumes as port congestion at Ho Chi Minh City and an erratic monsoon trim deliverable Q3 lots, pushing London futures to a nine-month high.

## H10
A semiconductor foundry confirms standard yield on a mature node.

## H11
Red Sea diversions keep Asia-to-Europe container rates elevated for apparel and furniture importers, with carriers adding a fourth week of transit time around the Cape of Good Hope and blanking two sailings on the Far East-Mediterranean loop.

## H12
Customs throughput at a southern border crossing matches seasonal norms.

## H13
A major smartphone assembler guides June-quarter unit shipments down about 6% on soft consumer demand in China and Western Europe, citing extended replacement cycles and elevated channel inventory of mid-tier handsets.
"""

NEW["pg_007"] = """# Earnings & Analyst Feed
# Date: 2026-06-15

## E01 — Generic Issuer
A logistics SPAC delists from a secondary exchange after low volume.

## E02 — Generic Issuer
An insurance broker prices a secondary follow-on offering at the midpoint.

## E03 — Generic Issuer
An asset manager reports inflows into infrastructure equity funds.

## E04 — Northern Pantry Markets
Northern Pantry Markets cuts FY guidance, citing comparable-store sales weakness. Analysts: market share likely to shift toward rival Sunbelt Grocers, which has been more aggressive on private label and may pull forward distribution capex.

## E05 — Generic Issuer
An auto-parts retailer posts comparable-store sales growth of 1.5%.

## E06 — Continental Air Group
A full-service airline trims full-year unit-revenue guidance, citing softening transatlantic leisure fares and higher regional-jet maintenance costs, while reaffirming its fleet-delivery schedule and summer capacity plan.

## E07 — Generic Issuer
A defense contractor reaffirms full-year backlog conversion guidance.

## E08 — Lumen Stream Media
A subscription streaming platform beats on net additions but guides margins lower, blaming content amortization and a stronger dollar on overseas ad sales; management reiterates a password-sharing crackdown timeline.

## E09 — Generic Issuer
A consumer-staples giant beats by a penny on routine volume growth.

## E10 — Generic Issuer
A regional bank affirms its quarterly outlook; loan-loss provisions trend flat.

## E11 — Cornerstone Residential
A national homebuilder reports orders up 4% as mortgage-rate buydowns support entry-level demand, but flags lumber and land-development cost inflation and a longer cycle time in two Sun Belt markets.

## E12 — Generic Issuer
A regional grocery chain raises its dividend in line with prior guidance.
"""

NEW["pg_008"] = """# Regulatory & Standards Feed
# Date: 2026-06-15

## R01
An obscure ISO subcommittee publishes a routine corrigendum to a packaging standard.

## R02
A trade publication summarizes mostly unchanged Q2 rule-comment filings.

## R03
A state public utility commission opens a docket on residential electrification.

## R04
OSHA finalizes a heat-illness prevention rule for outdoor workers.

## R05
A securities regulator finalizes expanded climate-disclosure rules requiring large filers to report Scope 1 and 2 emissions and material transition risks in annual filings, with phased assurance requirements beginning in fiscal 2027 for accelerated filers.

## R06
Executive Order 14102 amended: light-duty passenger-vehicle CO2 fleet-average targets tighten 18% by MY2028. Affected manufacturers and Tier-1 powertrain suppliers must refile compliance plans within 120 days.

## R07
A federal advisory committee reschedules a meeting on telework records retention.

## R08
EU customs publishes a detailed classification notice reassigning several non-related polymer grades between tariff headings, clarifying duty treatment for consumer packaging films and instructing importers to update binding tariff information references before the autumn quota window.

## R09
A regional water board updates its lab method for a non-related contaminant.

## R10
A banking supervisor opens consultation on revised capital treatment for operational risk, proposing a standardized business-indicator approach and higher output floors that would phase in over three years for internationally active lenders.

## R11
The IRS publishes guidance on R&D capitalization that does not affect our segment.

## R12
FCC dismisses a low-priority petition on amateur radio band sharing.
"""

NEW["pg_009"] = """# Infrastructure & Operations Feed
# Date: 2026-06-15

## I01
A regional cloud operator publishes a routine sustainability update on water-use reporting.

## I02
Two undersea fiber routes serving East Asia report simultaneous repair windows after a seismic survey flags cable abrasion near a busy landing corridor. Service providers say customers are reviewing regional capacity placement, but no downstream operational changes have been announced.

## I03
A consumer storage distributor warns of a second-half price war in retail NAND as oversupply of low-end USB drives and SD cards meets weak back-to-school demand, with spot prices for older 3D-NAND grades down sharply quarter over quarter.

## I04
A telecom carrier delays a metro fiber lighting ceremony because of municipal permitting paperwork.

## I05
A contract manufacturer reports that a Southeast Asia surface-mount line resumed full output after a scheduled maintenance shutdown, adding that yield on its mature consumer-board program held steady and that no customer ship dates moved.

## I06
A standards body opens comments on liquid-cooling terminology for high-density racks.

## I07
A hyperscale leasing broker reports the strongest quarter on record for Midwest powered-shell sites as tenants pre-lease shell capacity 18 months out, pushing headline rents up and shrinking available megawatts in two secondary metros.

## I08
An optical transceiver vendor announces a minor firmware advisory for lab validation equipment.

## I09
A game-streaming platform reports record weekend concurrency after a marquee title launch, peaking near 4 million simultaneous viewers and prompting a temporary regional load-balancing change that it says has since returned to normal.

## I10
A port authority says container dwell times for electronics cargo are within seasonal ranges.
"""


def main() -> None:
    f = pathlib.Path(__file__).resolve().parent.parent / "cases.jsonl"
    rows = [json.loads(l) for l in f.read_text().splitlines() if l.strip()]
    for r in rows:
        cid = r["id"]
        if cid not in NEW:
            continue
        old = r["inputs"]["event_stream"]
        new = NEW[cid].rstrip("\n") + "\n"
        assert old in r["inputs"]["prompt"], f"{cid}: event_stream not found verbatim in prompt"
        # event count must be preserved (oracle ids unchanged)
        assert old.count("\n## ") == new.count("\n## "), f"{cid}: event count changed"
        r["inputs"]["prompt"] = r["inputs"]["prompt"].replace(old, new)
        r["inputs"]["event_stream"] = new
    f.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n")
    print(f"rewrote feeds for {len(NEW)} cases")


if __name__ == "__main__":
    main()
