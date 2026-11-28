#!/usr/bin/env python3
"""Validate the x86-64 instruction registry and generate its Rust tables.

  validate   check crates/loader/x86/instructions.toml
  generate   validate, then write crates/loader/src/x86_table.rs
  --check    validate and regenerate in memory; exit 1 if the committed file
             differs. Never writes.

Rejects unknown keys and vocabulary, overlapping encodings, digit and plain
forms sharing an opcode, operand constraints that contradict each other, a
fixed r/m field on a form that could take memory, LOCK on a form that cannot
write memory, and flags declared both written and undefined. Standard library only (Python 3.11+).
"""

import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:
    sys.exit("generate_x86.py needs Python 3.11 or newer (tomllib)")

HERE = Path(__file__).resolve().parent
REGISTRY = HERE / "x86/instructions.toml"
RUST = HERE / "src/x86_table.rs"

TOP_KEYS = {"schema", "architecture", "documents", "operation", "alu", "form", "group", "vex"}
OPERATION_KEYS = {"reads", "writes", "undefined", "conditional", "terminator", "faults", "ref"}
FORM_KEYS = {"op", "map", "prefix", "opcode", "count", "digit", "enc", "size", "imm", "src",
             "memory_only", "register_only", "rm", "no_rex_b", "unaligned", "lock"}
VEX_KEYS = {"op", "map", "prefix", "opcode", "digit", "enc", "length", "w", "src", "nds",
            "memory_only", "register_only", "aligned"}
GROUP_KEYS = {"map", "opcodes", "digits", "lock"}
FLAGS = ["cf", "zf", "sf", "of", "pf", "af"]
PREFIXES = {"", "66", "f3", "f2"}
# Opcode table per opcode map and mandatory prefix. No other combination has a
# table, so a form cannot use it.
TABLES = {("1", ""): "ONE_BYTE", ("0f", ""): "TWO_BYTE", ("0f", "66"): "TWO_BYTE_66",
          ("0f", "f3"): "TWO_BYTE_F3", ("0f", "f2"): "TWO_BYTE_F2",
          ("0f38", "66"): "THREE_BYTE_38_66", ("0f38", "f2"): "THREE_BYTE_38_F2",
          ("0f3a", "66"): "THREE_BYTE_3A_66"}
MAPS = {opcode_map for opcode_map, _ in TABLES}
SIZES = {"b": "Byte", "v": "Variable", "q": "Quad", "y": "Wide", "n": "None", "x": "Xmm"}
IMMS = {"": "None", "ib": "SignedByte", "iz": "SignedFull", "iv": "Full", "iw": "Word", "ub": "Byte",
        "rel8": "SignedByte", "rel32": "Signed32", "moffs": "Offset"}
SRCS = {"": 0, "b": 1, "w": 2, "d": 4, "q": 8, "x": 16}
# encoding -> (Rust variant, has ModRM, required immediates)
ENCS = {
    "none": ("None", False, {""}),
    "string": ("String", False, {""}),
    "rm_reg": ("RmReg", True, {""}),
    "reg_rm": ("RegRm", True, {""}),
    "reg_rm_imm": ("RegRmImm", True, {"ib", "iz"}),
    "rm": ("Rm", True, {""}),
    "rm_only": ("Rm", True, {""}),
    "rm_unread": ("RmUnread", True, {""}),
    "rm_imm": ("RmImm", True, {"ib", "iz"}),
    "rm_one": ("RmOne", True, {""}),
    "rm_cl": ("RmCl", True, {""}),
    "rm_reg_imm": ("RmRegImm", True, {"ub"}),
    "rm_reg_cl": ("RmRegCl", True, {""}),
    "acc_imm": ("AccImm", False, {"ib", "iz"}),
    # The accumulator and an absolute address in place of ModRM (MOV A0-A3).
    "acc_moffs": ("RegRm", False, {"moffs"}),
    "moffs_acc": ("RmReg", False, {"moffs"}),
    "op_reg": ("OpReg", False, {""}),
    "op_reg_imm": ("OpRegImm", False, {"ib", "iv"}),
    "imm": ("Imm", False, {"ib", "iz", "iw"}),
    "rel": ("Rel", False, {"rel8", "rel32"}),
    "xmm_rm": ("XmmRm", True, {""}),
    "rm_xmm": ("RmXmm", True, {""}),
    "xmm_rm_imm": ("XmmRmImm", True, {"ub"}),
    # The operand size is that of the length registers.
    "xmm_rm_length": ("XmmRmLength", True, {"ub"}),
    "rm_xmm_imm": ("RmXmmImm", True, {"ub"}),
    "rm_imm_xmm": ("RmImmXmm", True, {"ub"}),
    "xmm_gpr": ("XmmGpr", True, {""}),
    "gpr_xmm": ("GprXmm", True, {""}),
    "xmm_gpr_imm": ("XmmGprImm", True, {"ub"}),
    "gpr_xmm_imm": ("GprXmmImm", True, {"ub"}),
    "reg_xmm": ("RegXmm", True, {""}),
    "reg_xmm_imm": ("RegXmmImm", True, {"ub"}),
    # BMI: general reg from the VEX.vvvv register and the ModRM operand.
    "reg_vvvv_rm": ("RegVvvvRm", True, {""}),
    # BMI: the VEX.vvvv register from the ModRM operand, under a digit.
    "vvvv_rm": ("VvvvRm", True, {""}),
    # RORX: general reg from the ModRM operand, with an unsigned byte count.
    "reg_rm_ub": ("RegRmImm", True, {"ub"}),
}
# VEX encodings: the operand size, and whether VEX.vvvv may name a register.
VEX_ENCS = {"none": ("n", False), "rm": ("n", False), "xmm_rm": ("x", True), "rm_xmm": ("x", True),
            "xmm_rm_imm": ("x", True), "rm_xmm_imm": ("x", False), "xmm_gpr": ("y", True),
            "gpr_xmm": ("y", False), "reg_xmm": ("y", False), "xmm_rm_length": ("y", False),
            "rm_imm_xmm": ("x", True), "xmm_gpr_imm": ("y", True), "gpr_xmm_imm": ("y", False),
            "reg_xmm_imm": ("y", False), "reg_vvvv_rm": ("y", True), "vvvv_rm": ("y", True),
            "reg_rm_ub": ("y", False)}
VEX_MAPS = {"0f": 1, "0f38": 2, "0f3a": 3}
VEX_PREFIXES = {"": 0, "66": 1, "f3": 2, "f2": 3}
VEX_LENGTHS = {"128": False, "256": True, "any": None}
VEX_WIDTHS = {"0": False, "1": True, "any": None}
# Memory width: the vector length, a number of bytes, or that of the general operand.
VEX_SRCS = {"v": 0, "b": 1, "w": 2, "d": 4, "q": 8, "x": 16, "": 255}
# Encodings whose ModRM operand is the destination, so LOCK can apply to it.
RM_DESTINATIONS = {"rm_reg", "rm", "rm_imm", "rm_one", "rm_cl", "rm_reg_imm", "rm_reg_cl"}
# Encodings whose ModRM operand is an XMM register or memory of the source width.
XMM_RM = {"xmm_rm", "rm_xmm", "xmm_rm_imm", "rm_xmm_imm", "xmm_rm_length", "rm_imm_xmm", "reg_xmm", "reg_xmm_imm"}
# Those whose operand size is that of a general register.
GENERAL_SIZED = {"xmm_rm_length", "reg_xmm", "reg_xmm_imm"}
# One-byte opcodes that are prefixes or the two-byte escape, never instructions.
RESERVED = {0x0F, 0x26, 0x2E, 0x36, 0x3E, 0x64, 0x65, 0x66, 0x67, 0xF0, 0xF2, 0xF3,
            *range(0x40, 0x50)}


class RegistryError(Exception):
    pass


def fail(message):
    raise RegistryError(message)


def check_keys(where, table, allowed):
    unknown = set(table) - allowed
    if unknown:
        fail(f"{where}: unknown keys {sorted(unknown)}")


def flag_set(where, text):
    names = text.split()
    for name in names:
        if name not in FLAGS:
            fail(f"{where}: unknown flag {name!r}")
    if len(set(names)) != len(names):
        fail(f"{where}: repeated flag")
    return set(names)


def check_lock(where, group, operations):
    for op in group.get("lock", ()):
        if op not in operations:
            fail(f"{where}: lock names {op}, which is not in the group")


def load(text):
    """Parse and validate the registry text, returning (operations, forms)."""
    data = tomllib.loads(text)
    check_keys("registry", data, TOP_KEYS)
    if data.get("schema") != 1 or data.get("architecture") != "x86-64":
        fail("registry: unsupported schema or architecture")
    documents = data["documents"]

    operations = {}
    for name, entry in data["operation"].items():
        where = f"operation {name}"
        check_keys(where, entry, OPERATION_KEYS)
        reads, writes, undefined = (
            flag_set(where, entry.get(key, "")) for key in ("reads", "writes", "undefined"))
        if writes & undefined:
            fail(f"{where}: flags both written and undefined: {sorted(writes & undefined)}")
        if entry.get("ref", " ").split()[0:1] == [] or entry.get("ref", "").split()[0] not in documents:
            fail(f"{where}: reference must name a listed document")
        operations[name] = {
            "reads": reads, "writes": writes, "undefined": undefined,
            "conditional": entry.get("conditional", False),
            "terminator": entry.get("terminator", False),
            "faults": entry.get("faults", False),
        }

    forms = [dict(form) for form in data["form"]]
    for form in forms:
        check_keys(f"form {form.get('op')}", form, FORM_KEYS)
    for index, op in enumerate(data["alu"]["operations"]):
        base = index * 8
        for offset, (enc, size, imm) in enumerate([
                ("rm_reg", "b", ""), ("rm_reg", "v", ""), ("reg_rm", "b", ""),
                ("reg_rm", "v", ""), ("acc_imm", "b", "ib"), ("acc_imm", "v", "iz")]):
            # Intel SDM Vol. 2, LOCK: every ALU operation but CMP may lock a
            # memory destination.
            forms.append({"op": op, "opcode": base + offset, "enc": enc, "size": size, "imm": imm,
                          "lock": op != "cmp" and enc == "rm_reg"})
        for opcode, size, imm in ((0x80, "b", "ib"), (0x81, "v", "iz"), (0x83, "v", "ib")):
            forms.append({"op": op, "opcode": opcode, "digit": index, "enc": "rm_imm",
                          "size": size, "imm": imm, "lock": op != "cmp"})
    for group in data["group"]:
        check_keys("group", group, GROUP_KEYS)
        check_lock("group", group, group["digits"].values())
        for opcode, size, imm, enc in group["opcodes"]:
            for digit, op in sorted(group["digits"].items()):
                forms.append({"op": op, "map": group.get("map", "1"), "opcode": opcode,
                              "digit": int(digit), "enc": enc, "size": size, "imm": imm,
                              "lock": op in group.get("lock", ())})

    claimed = {}
    used = set()
    for form in forms:
        form.setdefault("map", "1")
        for key in ("prefix", "imm", "src"):
            form.setdefault(key, "")
        form.setdefault("count", 1)
        form.setdefault("digit", None)
        where = f"form {form['op']} {form['map']}:{form['opcode']:02x}"
        for key, allowed in (("op", operations), ("map", MAPS), ("prefix", PREFIXES),
                             ("enc", ENCS), ("size", SIZES), ("imm", IMMS), ("src", SRCS)):
            if form[key] not in allowed:
                fail(f"{where}: unknown {key} {form[key]!r}")
        used.add(form["op"])
        _, modrm, imms = ENCS[form["enc"]]
        if form["imm"] not in imms:
            fail(f"{where}: immediate {form['imm']!r} is not valid for {form['enc']}")
        if form["imm"] == "ib" and form["size"] == "n":
            fail(f"{where}: a sized immediate needs an operand size")
        if form["digit"] is not None and (not modrm or not 0 <= form["digit"] <= 7):
            fail(f"{where}: a digit needs a ModRM encoding and a value 0-7")
        if form.get("memory_only") and form.get("register_only"):
            fail(f"{where}: memory_only and register_only contradict")
        if (form.get("memory_only") or form.get("register_only")) and not modrm:
            fail(f"{where}: operand restriction without ModRM")
        form.setdefault("rm", None)
        if form["rm"] is not None:
            # bool is an int in Python; `rm = true` is not a field value.
            if type(form["rm"]) is not int or not 0 <= form["rm"] <= 7:
                fail(f"{where}: rm must be a ModRM r/m field value 0-7")
            if not form.get("register_only") or form["digit"] is None:
                fail(f"{where}: a fixed r/m field needs register_only and a digit")
        # A general register or memory exchanged with a vector lane: fixed ones
        # name their memory width, wide ones are 4 or 8 bytes by REX.W.
        lane = form["enc"] in ("xmm_gpr_imm", "gpr_xmm_imm")
        sourced = form["enc"] in XMM_RM
        if sourced != bool(form["src"]) and form["enc"] not in ("reg_rm", "rm") and not lane:
            fail(f"{where}: source size does not match the encoding")
        if form["src"] and not sourced and form["enc"] == "rm" and form["size"] != "n":
            fail(f"{where}: source size does not match the encoding")
        if lane and (form["size"] not in ("x", "y") or (form["size"] == "x") != (form["src"] in ("b", "w", "d"))):
            fail(f"{where}: a lane is a byte, word or doubleword by source, or wide by size")
        if not lane and (form["size"] == "x") != (form["enc"] in XMM_RM - GENERAL_SIZED):
            fail(f"{where}: XMM size does not match the encoding")
        if form.get("lock") and (form["enc"] not in RM_DESTINATIONS or form.get("register_only")):
            fail(f"{where}: LOCK needs a memory destination")
        if form.get("unaligned") and form["src"] != "x":
            fail(f"{where}: alignment applies only to 16-byte operands")
        if (form["map"], form["prefix"]) not in TABLES:
            fail(f"{where}: no opcode table for prefix {form['prefix']!r} in map {form['map']}")
        if form["count"] not in (1, 8, 16) or form["opcode"] % form["count"]:
            fail(f"{where}: count must be 1, 8 or 16 and aligned")
        if form["count"] == 8 and form["enc"] not in ("op_reg", "op_reg_imm"):
            fail(f"{where}: count 8 encodes a register")
        if form["count"] == 16 and form["op"] not in ("jcc", "cmov", "setcc"):
            fail(f"{where}: count 16 encodes a condition")
        for opcode in range(form["opcode"], form["opcode"] + form["count"]):
            if not 0 <= opcode <= 0xFF:
                fail(f"{where}: opcode out of range")
            if form["map"] == "1" and opcode in RESERVED:
                fail(f"{where}: {opcode:02x} is a prefix or escape")
            slots = claimed.setdefault((form["map"], form["prefix"], opcode), {})
            if form["digit"] in slots or (slots and (None in slots or form["digit"] is None)):
                fail(f"{where}: overlapping encoding")
            slots[form["digit"]] = form
            # 0x66 selects a 16-bit operand for an unprefixed integer form, so a
            # 66-prefixed form at the same opcode would be ambiguous.
            for other, sized in (("66", form), ("", None)):
                if form["map"] == "0f" and form["prefix"] == ("" if other == "66" else "66"):
                    for rival in claimed.get(("0f", other, opcode), {}).values():
                        plain = sized or rival
                        if plain["size"] in ("b", "v"):
                            fail(f"{where}: 0x66 is both an operand size and a mandatory prefix")
            if form["digit"] is not None:
                first = next(iter(slots.values()))
                if (first["size"] in "bn") != (form["size"] in "bn"):
                    fail(f"{where}: group mixes byte and wider operands")
    for form in data.get("vex", ()):
        forms.append(load_vex(form, operations))
        used.add(form["op"])
    overlapping = {}
    for form in forms:
        if "vex" not in form:
            continue
        vex, rivals = form["vex"], overlapping.setdefault(form["vex"]["key"], [])
        for rival in rivals:
            if all(a is None or b is None or a == b for a, b in (
                    (vex["digit"], rival["vex"]["digit"]), (vex["length"], rival["vex"]["length"]),
                    (vex["w"], rival["vex"]["w"]), (mode(form), mode(rival)))):
                fail(f"vex {form['op']} {form['map']}:{form['opcode']:02x}: overlapping encoding")
        rivals.append(form)
    unused = set(operations) - used
    if unused:
        fail(f"operations without forms: {sorted(unused)}")
    return operations, forms


def mode(form):
    """Whether a form takes only memory, only a register, or either (None)."""
    return True if form["memory_only"] else False if form["register_only"] else None


def load_vex(entry, operations):
    """Validate a VEX entry, returning a form with its VEX rule."""
    form = dict(entry)
    where = f"vex {form.get('op')} {form.get('map')}:{form.get('opcode', 0):02x}"
    check_keys(where, form, VEX_KEYS)
    form.setdefault("prefix", "")
    form.setdefault("src", "")
    form.setdefault("w", "any")
    form.setdefault("digit", None)
    for key in ("nds", "memory_only", "register_only", "aligned"):
        form[key] = bool(form.get(key))
    for key, allowed in (("op", operations), ("map", VEX_MAPS), ("prefix", VEX_PREFIXES),
                         ("enc", VEX_ENCS), ("length", VEX_LENGTHS), ("w", VEX_WIDTHS),
                         ("src", VEX_SRCS)):
        if form.get(key) not in allowed:
            fail(f"{where}: unknown {key} {form.get(key)!r}")
    if not 0 <= form["opcode"] <= 0xFF:
        fail(f"{where}: opcode out of range")
    size, sources = VEX_ENCS[form["enc"]]
    if form["nds"] and not sources:
        fail(f"{where}: {form['enc']} takes no register from VEX.vvvv")
    if form["memory_only"] and form["register_only"]:
        fail(f"{where}: memory_only and register_only contradict")
    modrm = ENCS[form["enc"]][1]
    if not modrm and (form["memory_only"] or form["register_only"] or form["digit"] is not None):
        fail(f"{where}: operand restriction without ModRM")
    if form["digit"] is not None and not 0 <= form["digit"] <= 7:
        fail(f"{where}: a digit is 0-7")
    # A lane exchanged with a general register or memory is a fixed width by
    # source, or that of the general operand.
    lane = form["enc"] in ("xmm_gpr_imm", "gpr_xmm_imm")
    if lane and form["src"] not in ("b", "w", "d", ""):
        fail(f"{where}: a lane is a byte, word, doubleword or the general operand")
    if not lane and (form["src"] == "") != (form["enc"] in ("none", "xmm_gpr", "gpr_xmm", "reg_vvvv_rm", "vvvv_rm", "reg_rm_ub")):
        fail(f"{where}: source size does not match the encoding")
    if form["aligned"] and form["src"] not in ("v", "x"):
        fail(f"{where}: alignment applies only to whole vectors")
    if form["enc"] == "rm_xmm" and form["nds"] and not form["register_only"]:
        fail(f"{where}: a store takes no source from VEX.vvvv")
    # A shift by an immediate writes VEX.vvvv and has its source in a register.
    if form["enc"] == "rm_imm_xmm" and not (form["nds"] and form["register_only"]):
        fail(f"{where}: {form['enc']} needs VEX.vvvv and a register operand")
    form.update(size=size, imm="ub" if ENCS[form["enc"]][2] == {"ub"} else "", count=1, rm=None,
                unaligned=not form["aligned"])
    form["vex"] = {
        "key": VEX_MAPS[form["map"]] << 10 | VEX_PREFIXES[form["prefix"]] << 8 | form["opcode"],
        "digit": form["digit"], "length": VEX_LENGTHS[form["length"]], "w": VEX_WIDTHS[form["w"]],
        "memory": VEX_SRCS[form["src"]], "nds": form["nds"]}
    return form


def camel(name):
    return name[0].upper() + name[1:]


def mask(flags):
    return sum(1 << FLAGS.index(flag) for flag in flags)


def generate(operations, forms):
    out = ["// SPDX-License-Identifier: GPL-3.0-or-later",
           "",
           "// Generated by crates/loader/generate_x86.py from crates/loader/x86/instructions.toml.",
           "// Do not edit.",
           "",
           "use crate::x86_decode::{Encoding, Entry, Form, Immediate, OperandSize, VexForm};",
           "",
           "/// Operations with decodable forms.",
           "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]",
           "pub enum Op {"]
    out += [f"    {camel(name)}," for name in operations]
    out += ["}", "", "impl Op {",
            "    pub const ALL: &[Op] = &["
            + ", ".join(f"Op::{camel(n)}" for n in operations) + "];", ""]

    def method(doc, name, kind, value):
        out.append(f"    /// {doc}")
        out.append(f"    pub const fn {name}(self) -> {kind} {{")
        out.append("        match self {")
        for op, entry in operations.items():
            out.append(f"            Op::{camel(op)} => {value(op, entry)},")
        out.extend(["        }", "    }", ""])

    method("Lower-case mnemonic stem.", "name", "&'static str", lambda op, e: f'"{op}"')
    bits = "Flag bits: 1 CF, 2 ZF, 4 SF, 8 OF, 16 PF, 32 AF."
    method(f"Flags read. {bits}", "flags_read", "u8", lambda op, e: mask(e["reads"]))
    method(f"Flags always written. {bits}", "flags_written", "u8",
           lambda op, e: 0 if e["conditional"] else mask(e["writes"]))
    method(f"Flags this operation may change. {bits}", "flags_touched", "u8",
           lambda op, e: mask(e["writes"]))
    method(f"Architecturally undefined flags. {bits}", "flags_undefined", "u8",
           lambda op, e: mask(e["undefined"]))
    method("Ends a translated block.", "is_terminator", "bool",
           lambda op, e: str(e["terminator"]).lower())
    method("Can raise an arithmetic fault.", "can_fault", "bool",
           lambda op, e: str(e["faults"]).lower())
    vector = {form["op"] for form in forms if form["size"] == "x" or "xmm" in form["enc"]}
    method("Uses an XMM register.", "is_vector", "bool", lambda op, e: str(op in vector).lower())
    out[-1:] = ["}", ""]

    out.append(f"pub static FORMS: [Form; {len(forms)}] = [")
    for form in forms:
        out.append(
            f"    Form {{ op: Op::{camel(form['op'])}, encoding: Encoding::{ENCS[form['enc']][0]}, "
            f"size: OperandSize::{SIZES[form['size']]}, immediate: Immediate::{IMMS[form['imm']]}, "
            f"source: {0 if 'vex' in form else SRCS[form['src']]}, has_modrm: {str(ENCS[form['enc']][1]).lower()}, "
            f"memory_only: {str(bool(form.get('memory_only'))).lower()}, "
            f"register_only: {str(bool(form.get('register_only'))).lower()}, "
            f"fixed_rm: {'None' if form['rm'] is None else 'Some(%d)' % form['rm']}, "
            f"no_rex_b: {str(bool(form.get('no_rex_b'))).lower()}, "
            f"unaligned: {str(bool(form.get('unaligned'))).lower()}, "
            f"lockable: {str(bool(form.get('lock'))).lower()} }},")
    out += ["];", ""]

    groups = []
    tables = {name: ["Entry::Unsupported"] * 256 for name in TABLES.values()}
    slots = {}
    for index, form in enumerate(forms):
        if "vex" in form:
            continue
        table = TABLES[form["map"], form["prefix"]]
        for opcode in range(form["opcode"], form["opcode"] + form["count"]):
            slots.setdefault((table, opcode), {})[form["digit"]] = index
    for (table, opcode), digits in sorted(slots.items()):
        if None in digits:
            tables[table][opcode] = f"Entry::Form({digits[None]})"
        else:
            row = [str(digits.get(d, 65535)) for d in range(8)]
            tables[table][opcode] = f"Entry::Group({len(groups)})"
            groups.append("[" + ", ".join(row) + "]")
    out.append("/// Form per ModRM digit; 65535 marks an unsupported digit.")
    out.append(f"pub static GROUPS: [[u16; 8]; {len(groups)}] = [")
    out += [f"    {row}," for row in groups]
    out += ["];", ""]
    ordered = sorted((form["vex"]["key"], index) for index, form in enumerate(forms) if "vex" in form)
    optional = lambda value: "None" if value is None else f"Some({str(value).lower()})"
    out.append("/// VEX-encoded forms by opcode map, mandatory prefix and opcode.")
    out.append(f"pub static VEX_FORMS: [VexForm; {len(ordered)}] = [")
    for key, index in ordered:
        vex = forms[index]["vex"]
        out.append(f"    VexForm {{ key: 0x{key:03x}, form: {index}, digit: {optional(vex['digit'])}, "
                   f"length: {optional(vex['length'])}, wide: {optional(vex['w'])}, "
                   f"memory: {vex['memory']}, nds: {str(vex['nds']).lower()} }},")
    out += ["];", ""]
    for name, entries in tables.items():
        out.append(f"pub static {name}: [Entry; 256] = [")
        for start in range(0, 256, 4):
            out.append("    " + ", ".join(entries[start:start + 4]) + ",")
        out += ["];", ""]
    return "\n".join(out[:-1]) + "\n"


def main(argv):
    if len(argv) != 2 or argv[1] not in ("validate", "generate", "--check"):
        sys.exit("usage: generate_x86.py validate|generate|--check")
    try:
        operations, forms = load(REGISTRY.read_text())
    except (RegistryError, KeyError, tomllib.TOMLDecodeError) as error:
        sys.exit(f"instructions.toml: {error}")
    rust = generate(operations, forms)
    if argv[1] == "generate":
        RUST.write_text(rust)
    elif argv[1] == "--check" and (not RUST.exists() or RUST.read_text() != rust):
        sys.exit("x86_table.rs is out of date; run generate_x86.py generate")
    encodings = sum(form["count"] for form in forms)
    print(f"{len(operations)} operations, {len(forms)} forms, {encodings} opcode slots")


if __name__ == "__main__":
    main(sys.argv)
