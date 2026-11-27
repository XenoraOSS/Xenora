# SPDX-License-Identifier: GPL-3.0-or-later
"""Read-only RDNA2 expansion planning from existing inventories.

Reuses scripts/audit/semantic_coverage.py loaders and the checked-in AMD
snapshot, semantic registry, generated tables and status evidence. Does not
edit GPU production code. Candidate declarations are never counted as
implemented forms.
"""
from __future__ import annotations

import json
import re
import runpy
import tomllib
from collections import defaultdict
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Iterable, Sequence


def _load_coverage_module(root: Path):
    path = root / "scripts" / "audit" / "semantic_coverage.py"
    return runpy.run_path(str(path))


@dataclass(frozen=True)
class FormRef:
    family: str
    name: str
    opcode: int

    @property
    def key(self) -> tuple[str, str]:
        return (self.family, self.name)


@dataclass
class Candidate:
    family: str
    name: str
    opcode: int
    width: int | None
    numeric_type: str | None
    operand_layout: str | None
    reusable_primitives: list[str] = field(default_factory=list)
    missing_operand_rules: list[str] = field(default_factory=list)
    missing_effects: list[str] = field(default_factory=list)
    spec_refs: list[dict[str, Any]] = field(default_factory=list)
    priority_batch: str = "unclassified"
    priority_rank: int = 99
    status: str = "unsupported"
    notes: list[str] = field(default_factory=list)


@dataclass
class Inventory:
    identified: int
    declared: int
    decoder_supported: int
    reference_verified: int
    metal_verified: int
    unsupported: int
    by_family: dict[str, dict[str, int]]


# Conventional GPU semantic-expansion file set for Codex manifests only.
# Planning does not claim or edit these paths.
DEFAULT_PERMITTED_FILES = (
    "crates/isa/semantics/rdna2.toml",
    "crates/isa/semantics/rdna2/",
    "crates/isa/src/semantics_gen.rs",
    "crates/decoder/src/lib.rs",
    "crates/shader/",
    "crates/ref/",
    "tests/validation/",
    "docs/status.toml",
    "CHANGELOG.md",
)

DEFAULT_ACCEPTANCE = (
    "cargo test --locked -p xenora-ref",
    "cargo test --locked -p xenora-semantic-review",
    "scripts/test-gpu.sh",
    "python3 scripts/audit/semantic_coverage.py --check",
)


def load_inventories(root: Path) -> dict[str, Any]:
    sc = _load_coverage_module(root)
    identified = sc["load_snapshot"](root)
    # The root registry includes per-family files; merge them as the coverage report does.
    registry = sc["load_registry"](root / sc["REGISTRY"])
    decoder = sc["parse_decoder"]((root / sc["DECODER"]).read_text(encoding="utf-8"))
    generated = sc["parse_generated"]((root / sc["GENERATED"]).read_text(encoding="utf-8"))
    decoder_forms = set(decoder) | set(generated)
    declared, rules = sc["read_declarations"](registry, identified)
    status = tomllib.loads((root / sc["STATUS"]).read_text(encoding="utf-8"))
    _, encoded, lowered, reference, metal = _evidence_sets(sc, status, identified)
    return {
        "sc": sc,
        "identified": identified,
        "registry": registry,
        "rules": rules,
        "declared": declared,
        "decoder": decoder_forms,
        "encoded": encoded,
        "lowered": lowered,
        "reference": reference,
        "metal": metal,
        "status": status,
    }


def _evidence_sets(sc, status, identified):
    encoded, lowered, reference, metal = set(), set(), set(), set()
    seen = set()
    for row in status["instruction"]:
        key = sc["status_form"](row, identified)
        if key in seen:
            continue
        seen.add(key)
        if row.get("llvm") == "three-way passed":
            encoded.add(key)
        lowering = row.get("lowering", "").strip().lower()
        if lowering.startswith(("msl ", "resolved ", "verified ", "constant or ", "per-thread ")):
            lowered.add(key)
        decodes = row.get("decoded") is True
        if decodes and row.get("reference") == "implemented" and row.get("cpu") == "fixture verified":
            reference.add(key)
        if decodes and row.get("gpu") in {"fixture verified", "fixture verified on Apple M5"}:
            metal.add(key)
    return seen, encoded, lowered, reference, metal


def summarize_inventory(data: dict[str, Any]) -> Inventory:
    identified = data["identified"]
    declared = data["declared"]
    decoder = data["decoder"]
    reference = data["reference"]
    metal = data["metal"]
    unsupported_keys = set(identified) - set(declared)
    by_family: dict[str, dict[str, int]] = {}
    for family, name in identified:
        bucket = by_family.setdefault(family, {
            "identified": 0, "declared": 0, "decoder": 0, "unsupported": 0,
            "reference": 0, "metal": 0,
        })
        bucket["identified"] += 1
        key = (family, name)
        if key in declared:
            bucket["declared"] += 1
        if key in decoder:
            bucket["decoder"] += 1
        if key in unsupported_keys:
            bucket["unsupported"] += 1
        if key in reference:
            bucket["reference"] += 1
        if key in metal:
            bucket["metal"] += 1
    return Inventory(
        identified=len(identified),
        declared=len(declared),
        decoder_supported=len(decoder),
        reference_verified=len(reference),
        metal_verified=len(metal),
        unsupported=len(unsupported_keys),
        by_family=dict(sorted(by_family.items())),
    )


def _declared_ops(registry: dict[str, Any]) -> set[str]:
    return {form.get("operation") for form in registry.get("form", []) if form.get("operation")}


def _batch_for(family: str, family_rule_names: list[str], ops: set[str]) -> tuple[str, int, str]:
    """Return (batch_id, priority_rank, status). Lower rank is higher priority."""
    if family_rule_names and ops:
        return f"batch-{family.lower()}-reuse-primitives", 0, "reuse-primitives"
    if family_rule_names:
        return f"batch-{family.lower()}-needs-primitives", 1, "needs-primitives"
    return f"batch-{family.lower()}-needs-operand-rules", 2, "needs-operand-rules"


def classify_unsupported(
    data: dict[str, Any],
    *,
    limit: int | None = None,
    families: Sequence[str] | None = None,
    statuses: Sequence[str] | None = None,
    exclude_families: Sequence[str] | None = None,
) -> list[Candidate]:
    """Build candidates for identified forms that are not registry-declared.

    Does not infer behaviour from the mnemonic. Width/type/layout are filled
    only when already present on related declared forms in the same family, or
    left null. Results are sorted by priority_rank then family/name.
    """
    identified = data["identified"]
    declared = data["declared"]
    registry = data["registry"]
    rules = data["rules"]
    ops = _declared_ops(registry)
    # A rule declared `variant_only` re-encodes declared forms (for example the
    # VOP3 encoding of a VOP2 form) and cannot host a new form of its family.
    variant_only_rules = {
        name for name, rule in registry.get("operand_rules", {}).items()
        if isinstance(rule, dict) and rule.get("variant_only") is True
    }
    # A rule used by only one direct form is evidence for that form, not for a
    # reusable family layout. This matters for narrow rules such as MED3's
    # three binary32 sources: treating it as a general VOP3 layout would make
    # unrelated integer and floating-point forms look implementation-ready.
    direct_rule_uses = defaultdict(int)
    for form in registry.get("form", []):
        if isinstance(form, dict) and isinstance(form.get("operands"), str):
            direct_rule_uses[form["operands"]] += 1
    form_specific_rules = {
        name for name, uses in direct_rule_uses.items() if uses < 2
    }
    family_widths: dict[str, set[int]] = defaultdict(set)
    family_types: dict[str, set[str]] = defaultdict(set)
    for form in registry.get("form", []):
        if "width" in form:
            family_widths[form["family"]].add(form["width"])
        if "signedness" in form:
            family_types[form["family"]].add(form["signedness"])

    family_filter = {f.upper() for f in families} if families else None
    status_filter = {s for s in statuses} if statuses else None
    excluded = {f.upper() for f in (exclude_families or ())}

    candidates: list[Candidate] = []
    unsupported = sorted(set(identified) - set(declared))
    for family, name in unsupported:
        if family_filter is not None and family.upper() not in family_filter:
            continue
        if family.upper() in excluded:
            continue
        opcode = identified[(family, name)]
        reusable = sorted(op for op in ops if op)
        family_rule_names = [
            n for n, (fam, _) in rules.items()
            if fam == family and n not in variant_only_rules and n not in form_specific_rules
        ]
        missing_rules = []
        if not family_rule_names:
            missing_rules.append(f"no operand_rules for family {family}")
        widths = sorted(family_widths.get(family, []))
        types = sorted(family_types.get(family, []))
        layout = family_rule_names[0] if len(family_rule_names) == 1 else None
        batch, rank, status = _batch_for(family, family_rule_names, ops)
        if status_filter is not None and status not in status_filter:
            continue
        candidates.append(Candidate(
            family=family,
            name=name,
            opcode=opcode,
            width=widths[0] if len(widths) == 1 else None,
            numeric_type=types[0] if len(types) == 1 else None,
            operand_layout=layout,
            reusable_primitives=reusable[:20],
            missing_operand_rules=missing_rules,
            missing_effects=["scc/vcc/exec effects unspecified until registry declaration"],
            spec_refs=[{"document": "xml", "note": "identity from pinned AMD snapshot only"}],
            priority_batch=batch,
            priority_rank=rank,
            status=status,
            notes=[
                "behaviour must come from the public AMD guide, never the mnemonic",
                "candidate is not implemented",
            ],
        ))
    candidates.sort(key=lambda c: (c.priority_rank, c.family, c.name, c.opcode))
    if limit is not None:
        candidates = candidates[:limit]
    return candidates


def plan_batches(candidates: list[Candidate]) -> dict[str, list[dict[str, Any]]]:
    batches: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for cand in candidates:
        batches[cand.priority_batch].append({
            "family": cand.family,
            "name": cand.name,
            "opcode": cand.opcode,
            "status": cand.status,
            "missing_operand_rules": cand.missing_operand_rules,
            "reusable_primitives": cand.reusable_primitives[:5],
        })
    return dict(sorted(batches.items()))


def discrepancies(data: dict[str, Any]) -> list[dict[str, str]]:
    """Registry vs decoder vs verification evidence mismatches."""
    issues: list[dict[str, str]] = []
    declared = data["declared"]
    decoder = data["decoder"]
    reference = data["reference"]
    metal = data["metal"]
    for key in sorted(declared - decoder):
        family, name = key
        issues.append({
            "kind": "declared-not-decoded",
            "family": family,
            "name": name,
            "detail": "registry declaration is not present in decoder SEMANTIC_FORMS/FORM_IDS",
        })
    for key in sorted(decoder - declared):
        family, name = key
        issues.append({
            "kind": "decoded-not-declared",
            "family": family,
            "name": name,
            "detail": "decoder lists a form that is not in the semantic registry",
        })
    for key in sorted(reference - declared):
        family, name = key
        issues.append({
            "kind": "reference-without-declaration",
            "family": family,
            "name": name,
            "detail": "status marks reference verification without a registry declaration",
        })
    for key in sorted(metal - reference):
        family, name = key
        issues.append({
            "kind": "metal-without-reference",
            "family": family,
            "name": name,
            "detail": "Metal verification without matching reference evidence",
        })
    return issues


def reconcile_with_coverage_report(root: Path, data: dict[str, Any]) -> dict[str, Any]:
    """Compare planner inventory to canonical semantic_coverage collect_coverage.

    Does not inflate planner counts. Reports per-family deltas only.
    """
    sc = data["sc"]
    identified = data["identified"]
    registry = data["registry"]
    decoder = data["decoder"]
    status = data["status"]
    counts, _rules = sc["collect_coverage"](identified, registry, decoder, status)
    # collect_coverage tuple per family:
    # Identified, Encoding-validated, Declared, Decoder-supported, IR, MSL,
    # Reference-verified, Metal-verified, Unsupported operand variants
    inv = summarize_inventory(data)
    mismatches: list[dict[str, Any]] = []
    for family, values in sorted(counts.items()):
        canon_identified, _enc, canon_declared, canon_decoder, _ir, _msl, canon_ref, canon_metal, _u = values
        bucket = inv.by_family.get(family, {})
        pairs = (
            ("identified", canon_identified, bucket.get("identified", 0)),
            ("declared", canon_declared, bucket.get("declared", 0)),
            ("decoder", canon_decoder, bucket.get("decoder", 0)),
            ("reference", canon_ref, bucket.get("reference", 0)),
            ("metal", canon_metal, bucket.get("metal", 0)),
        )
        for field_name, canon, plan in pairs:
            if canon != plan:
                mismatches.append({
                    "family": family,
                    "field": field_name,
                    "canonical": canon,
                    "planner": plan,
                    "delta": plan - canon,
                })
    return {
        "ok": not mismatches,
        "canonical_totals": {
            "identified": sum(v[0] for v in counts.values()),
            "declared": sum(v[2] for v in counts.values()),
            "decoder_supported": sum(v[3] for v in counts.values()),
            "reference_verified": sum(v[6] for v in counts.values()),
            "metal_verified": sum(v[7] for v in counts.values()),
        },
        "planner_totals": {
            "identified": inv.identified,
            "declared": inv.declared,
            "decoder_supported": inv.decoder_supported,
            "reference_verified": inv.reference_verified,
            "metal_verified": inv.metal_verified,
        },
        "mismatches": mismatches,
        "note": "Planner counts are not adjusted to match; mismatches must be investigated.",
    }


@dataclass(frozen=True)
class ClaimScope:
    """One active claim. Families exclude those families; path-only claims
    block candidates whose default permitted files overlap the claimed paths.
    A claim that lists both families and paths excludes only the families —
    the paths document the claim and do not blanket-block other families.
    """
    families: frozenset[str] = frozenset()
    paths: frozenset[str] = frozenset()


def parse_claims_file(path: Path) -> tuple[set[str], set[str]]:
    """Load excluded families and path-only claim prefixes.

    Expected shape: {"claims":[{"families":["VOP2"], "paths":["crates/isa/**"]}, ...]}
    or a bare list of such objects.

    Returns (exclude_families, path_only_claimed_paths). Path prefixes from
    claims that also name families are ignored for blocking so a SOP2+isa
    claim does not suppress every other family's manifests.
    """
    scopes = load_claim_scopes(path)
    families: set[str] = set()
    path_only: set[str] = set()
    for scope in scopes:
        families.update(scope.families)
        if scope.families:
            continue
        path_only.update(scope.paths)
    return families, path_only


def load_claim_scopes(path: Path) -> list[ClaimScope]:
    raw = json.loads(path.read_text(encoding="utf-8"))
    items = raw["claims"] if isinstance(raw, dict) and "claims" in raw else raw
    if not isinstance(items, list):
        raise ValueError("claims file must be a list or {\"claims\": [...]}")
    scopes: list[ClaimScope] = []
    for item in items:
        fams = frozenset(str(f).upper() for f in (item.get("families") or []))
        paths = frozenset(str(p) for p in (item.get("paths") or []))
        if not fams and not paths:
            continue
        scopes.append(ClaimScope(families=fams, paths=paths))
    return scopes


def claims_block_candidate(cand: Candidate, claimed_paths: set[str]) -> bool:
    """True if a path-only claim overlaps the default GPU expansion files."""
    if not claimed_paths:
        return False
    for permitted in DEFAULT_PERMITTED_FILES:
        for claimed in claimed_paths:
            if _path_overlap(permitted, claimed):
                return True
    return False


def _path_overlap(a: str, b: str) -> bool:
    a = a.rstrip("/")
    b = b.rstrip("/")
    if a.endswith("/**"):
        a = a[:-3].rstrip("/")
    if b.endswith("/**"):
        b = b[:-3].rstrip("/")
    return a == b or a.startswith(b + "/") or b.startswith(a + "/")


def build_manifests(
    candidates: list[Candidate],
    *,
    exclude_families: Sequence[str] | None = None,
    claimed_paths: Sequence[str] | None = None,
    limit: int | None = None,
) -> list[dict[str, Any]]:
    """Deterministic Codex-ready implementation manifests.

    Prioritises reuse-primitives, then needs-primitives, then needs-operand-rules.
    Excludes active claim families and path-only claims. Does not invent semantics.
    """
    excluded = {f.upper() for f in (exclude_families or ())}
    claimed = set(claimed_paths or ())
    manifests: list[dict[str, Any]] = []
    for cand in candidates:
        if cand.family.upper() in excluded:
            continue
        if claims_block_candidate(cand, claimed):
            continue
        deps: list[str] = []
        if cand.status == "needs-operand-rules":
            deps.append(f"define operand_rules for family {cand.family}")
        if cand.status == "needs-primitives":
            deps.append("add registry operation primitive before declaring this form")
        if cand.status == "reuse-primitives":
            deps.append("reuse existing registry primitives/operand rules; declare form from AMD guide")
        deps.append("do not infer behaviour from the mnemonic")
        manifest = {
            "id": f"{cand.family}:{cand.name}",
            "instruction": {
                "family": cand.family,
                "name": cand.name,
                "opcode": cand.opcode,
            },
            "priority_rank": cand.priority_rank,
            "status": cand.status,
            "dependencies": deps,
            "specification_references": cand.spec_refs,
            "permitted_files": list(DEFAULT_PERMITTED_FILES),
            "acceptance_tests": list(DEFAULT_ACCEPTANCE),
            "notes": list(cand.notes),
        }
        manifests.append(manifest)
        if limit is not None and len(manifests) >= limit:
            break
    return manifests


def coverage_diff(before: Inventory, after: Inventory) -> dict[str, Any]:
    return {
        "identified_delta": after.identified - before.identified,
        "declared_delta": after.declared - before.declared,
        "decoder_delta": after.decoder_supported - before.decoder_supported,
        "unsupported_delta": after.unsupported - before.unsupported,
        "reference_delta": after.reference_verified - before.reference_verified,
        "metal_delta": after.metal_verified - before.metal_verified,
        "before": asdict(before),
        "after": asdict(after),
    }


def dumps_candidates(candidates: list[Candidate]) -> str:
    payload = {
        "note": "Candidates are planning aids only; they are not implemented forms.",
        "count": len(candidates),
        "candidates": [asdict(c) for c in candidates],
    }
    return json.dumps(payload, indent=2, sort_keys=True) + "\n"


def dumps_inventory(inv: Inventory) -> str:
    return json.dumps(asdict(inv), indent=2, sort_keys=True) + "\n"


def compact_inventory(inv: Inventory) -> str:
    lines = [
        f"identified={inv.identified} declared={inv.declared} "
        f"decoder={inv.decoder_supported} unsupported={inv.unsupported} "
        f"ref={inv.reference_verified} metal={inv.metal_verified}",
    ]
    for family, bucket in inv.by_family.items():
        if bucket.get("unsupported", 0) == 0 and bucket.get("declared", 0) == 0:
            continue
        lines.append(
            f"  {family}: id={bucket['identified']} decl={bucket['declared']} "
            f"dec={bucket['decoder']} unsup={bucket['unsupported']}"
        )
    return "\n".join(lines) + "\n"


def compact_candidates(candidates: Sequence[Candidate]) -> str:
    lines = [f"candidates={len(candidates)}"]
    for cand in candidates:
        lines.append(
            f"  [{cand.priority_rank}:{cand.status}] {cand.family}:{cand.name} "
            f"op=0x{cand.opcode:x} batch={cand.priority_batch}"
        )
    return "\n".join(lines) + "\n"


def compact_manifests(manifests: Sequence[dict[str, Any]]) -> str:
    lines = [f"manifests={len(manifests)}"]
    for item in manifests:
        inst = item["instruction"]
        lines.append(
            f"  [{item['priority_rank']}:{item['status']}] {inst['family']}:{inst['name']} "
            f"deps={len(item['dependencies'])}"
        )
    return "\n".join(lines) + "\n"


def write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
