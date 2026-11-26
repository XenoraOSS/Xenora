# SPDX-License-Identifier: GPL-3.0-or-later
"""Unit tests for crates/isa/generate_semantics.py.

Each rejection rule is fed a small in-memory registry that breaks exactly that
rule, checked against a tiny hand-written XML snapshot. The real registry and
snapshot are only read, never edited. Run from the repository root:

    python3 -m unittest discover -s tests/semantics_generator
"""

import copy
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "crates/isa/generate_semantics.py"

_spec = importlib.util.spec_from_file_location("generate_semantics", SCRIPT)
gen = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gen)

SHA = "0" * 64

# Minimal stand-in for crates/isa/data/rdna2.jsonl: the SOP2 encoding and the
# default variants of the four forms the fixture declares (values copied from
# the AMD XML snapshot).
U32 = "FMT_NUM_U32"
I32 = "FMT_NUM_I32"


def _operands(data_format, scc_in=False, src1_format=None, scc_out=True):
    rows = [
        ["SDST", data_format, "OPR_SDST", 32, False, True, False, True],
        ["SSRC0", data_format, "OPR_SSRC_NOLDS", 32, True, False, False, True],
        ["SSRC1", src1_format or data_format, "OPR_SSRC_NOLDS", 32, True, False, False, True],
    ]
    if scc_out:
        rows.append(["", "FMT_NUM_B1", "OPR_SSRC_SPECIAL_SCC", 1, False, True, True, True])
    if scc_in:
        rows.append(["", "FMT_NUM_B1", "OPR_SSRC_SPECIAL_SCC", 1, True, False, True, True])
    return rows


def snapshot():
    sop2_base = 0x80000000
    encoding = {
        "name": "ENC_SOP2",
        "bits": 32,
        "fields": [
            ["SSRC0", [[0, 8]]], ["SSRC1", [[8, 8]]], ["SDST", [[16, 7]]],
            ["OP", [[23, 7]]], ["ENCODING", [[30, 2]]],
        ],
        "identifier_mask": 0xFF800000,
        "identifiers": [
            sop2_base | (op << 23) for op in (0, 2, 4, 6, 10, 14, 34, 38, 40, 47, 53)
        ],
    }
    variants = {}
    for name, opcode, operands in (
        ("S_ADD_U32", 0, _operands(U32)),
        ("S_ADD_I32", 2, _operands(I32)),
        ("S_ADDC_U32", 4, _operands(U32, scc_in=True)),
        ("S_AND_B32", 14, _operands(U32)),
        # The XML gives S_ASHR_I32 a signed S0 and an unsigned shift count.
        ("S_ASHR_I32", 34, _operands(I32, src1_format=U32)),
        # Values copied from the AMD XML snapshot for the arithmetic fixture.
        ("S_MIN_I32", 6, _operands(I32)),
        ("S_CSELECT_B32", 10, _operands("FMT_NUM_B32", scc_in=True, scc_out=False)),
        ("S_MUL_I32", 38, _operands(I32, scc_out=False)),
        ("S_BFE_I32", 40, _operands(I32, src1_format=U32)),
        ("S_LSHL2_ADD_U32", 47, _operands(U32)),
        ("S_MUL_HI_U32", 53, _operands(U32, scc_out=False)),
    ):
        variants[(name, "ENC_SOP2")] = {
            "instruction": name, "encoding": "ENC_SOP2", "opcode": opcode, "operands": operands,
        }
    sopc = {
        "name": "ENC_SOPC",
        "bits": 32,
        "fields": [
            ["SSRC0", [[0, 8]]], ["SSRC1", [[8, 8]]], ["OP", [[16, 7]]], ["ENCODING", [[23, 9]]],
        ],
        "identifier_mask": 0xFFFF0000,
        "identifiers": [0xBF000000 | (op << 16) for op in (4, 10)],
    }
    for name, opcode, data_format in (("S_CMP_LT_I32", 4, I32), ("S_CMP_LT_U32", 10, U32)):
        variants[(name, "ENC_SOPC")] = {
            "instruction": name, "encoding": "ENC_SOPC", "opcode": opcode,
            "operands": _operands(data_format)[1:],
        }
    sop1 = {
        "name": "ENC_SOP1",
        "bits": 32,
        "fields": [
            ["SSRC0", [[0, 8]]], ["OP", [[8, 8]]], ["SDST", [[16, 7]]], ["ENCODING", [[23, 9]]],
        ],
        "identifier_mask": 0xFF80FF00,
        "identifiers": [0xBE800000 | (op << 8) for op in (7, 11, 25, 60)],
    }

    def unary(dst, src, size, scc):
        rows = [
            ["SDST", dst, "OPR_SDST", 32, False, True, False, True],
            ["SSRC0", src, "OPR_SSRC_NOLDS", size, True, False, False, True],
        ]
        if scc:
            rows.append(["", "FMT_NUM_B1", "OPR_SSRC_SPECIAL_SCC", 1, False, True, True, True])
        return rows

    def saveexec(src, size):
        # The EXEC-writing forms of guide 12.3: the XML lists the mask as an
        # implicit read and an implicit write, next to the SCC output.
        return [
            ["SDST", "FMT_NUM_U32", "OPR_SREG", 32, False, True, False, True],
            ["SSRC0", src, "OPR_SSRC_NOLDS", size, True, False, False, True],
            ["", "FMT_NUM_M64", "OPR_SDST_EXEC", 64, True, False, True, True],
            ["", "FMT_NUM_B1", "OPR_SSRC_SPECIAL_SCC", 1, False, True, True, True],
            ["", "FMT_NUM_M64", "OPR_SDST_EXEC", 64, False, True, True, True],
        ]

    for name, opcode, operands in (
        ("S_NOT_B32", 7, unary(U32, U32, 32, True)),
        ("S_BREV_B32", 11, unary(U32, U32, 32, False)),
        ("S_SEXT_I32_I8", 25, unary(I32, "FMT_NUM_I8", 8, False)),
        ("S_AND_SAVEEXEC_B32", 60, saveexec(U32, 32)),
    ):
        variants[(name, "ENC_SOP1")] = {
            "instruction": name, "encoding": "ENC_SOP1", "opcode": opcode, "operands": operands,
        }
    vop2 = {
        "name": "ENC_VOP2",
        "bits": 32,
        "fields": [
            ["SRC0", [[0, 9]]], ["VSRC1", [[9, 8]]], ["VDST", [[17, 8]]], ["OP", [[25, 6]]],
            ["ENCODING", [[31, 1]]],
        ],
        "identifier_mask": 0xFE000000,
        "identifiers": [op << 25 for op in (9, 17, 39)],
    }

    def vector(data_format):
        return [
            ["VDST", data_format, "OPR_VGPR", 32, False, True, False, True],
            ["SRC0", data_format, "OPR_SRC", 32, True, False, False, True],
            ["VSRC1", data_format, "OPR_VGPR", 32, True, False, False, True],
        ]

    for name, opcode, data_format in (
        ("V_MUL_I32_I24", 9, I32), ("V_MIN_I32", 17, I32), ("V_SUBREV_NC_U32", 39, U32),
    ):
        variants[(name, "ENC_VOP2")] = {
            "instruction": name, "encoding": "ENC_VOP2", "opcode": opcode,
            "operands": vector(data_format),
        }
    vopc = {
        "name": "ENC_VOPC",
        "bits": 32,
        "fields": [
            ["SRC0", [[0, 9]]], ["VSRC1", [[9, 8]]], ["OP", [[17, 8]]], ["ENCODING", [[25, 7]]],
        ],
        "identifier_mask": 0xFFFE0000,
        "identifiers": [0x7C000000 | (op << 17) for op in (129, 135, 192, 197)],
    }
    for name, opcode, data_format in (
        ("V_CMP_LT_I32", 129, I32), ("V_CMP_T_I32", 135, I32), ("V_CMP_F_U32", 192, U32),
        ("V_CMP_NE_U32", 197, U32),
    ):
        variants[(name, "ENC_VOPC")] = {
            "instruction": name, "encoding": "ENC_VOPC", "opcode": opcode,
            # Values copied from the AMD XML snapshot: an implicit 64-bit VCC output.
            "operands": [
                ["", "FMT_NUM_M64", "OPR_VCC", 64, False, True, False, False],
                ["SRC0", data_format, "OPR_SRC", 32, True, False, False, True],
                ["VSRC1", data_format, "OPR_VGPR", 32, True, False, False, True],
            ],
        }
    vop3 = {
        "name": "ENC_VOP3",
        "bits": 64,
        "fields": [
            ["VDST", [[0, 8]]], ["ABS", [[8, 3]]], ["OP_SEL", [[11, 4]]], ["CLAMP", [[15, 1]]],
            ["OP", [[16, 10]]], ["ENCODING", [[26, 6]]], ["SRC0", [[32, 9]]], ["SRC1", [[41, 9]]],
            ["SRC2", [[50, 9]]], ["OMOD", [[59, 2]]], ["NEG", [[61, 3]]],
        ],
        "identifier_mask": 0xFFFF0000,
        "identifiers": [0xD4000000 | (op << 16) for op in (129, 265, 273, 295)],
    }
    # Values copied from the AMD XML snapshot: VOP3 encodings of VOP2 forms.
    for name, opcode, data_format in (
        ("V_MUL_I32_I24", 265, I32), ("V_MIN_I32", 273, I32), ("V_SUBREV_NC_U32", 295, U32),
    ):
        variants[(name, "ENC_VOP3")] = {
            "instruction": name, "encoding": "ENC_VOP3", "opcode": opcode,
            "operands": [
                ["VDST", data_format, "OPR_VGPR", 32, False, True, False, True],
                ["SRC0", data_format, "OPR_SRC", 32, True, False, False, True],
                ["SRC1", data_format, "OPR_SRC_NOLDS", 32, True, False, False, True],
            ],
        }
    variants[("V_CMP_LT_I32", "ENC_VOP3")] = {
        "instruction": "V_CMP_LT_I32", "encoding": "ENC_VOP3", "opcode": 129,
        "operands": [
            ["VDST", "FMT_NUM_M64", "OPR_SREG", 64, False, True, False, True],
            ["SRC0", I32, "OPR_SRC", 32, True, False, False, True],
            ["SRC1", I32, "OPR_SRC_NOLDS", 32, True, False, False, True],
        ],
    }
    return {
        "source": {"sha256": SHA},
        "encodings": {
            "ENC_SOP2": encoding, "ENC_SOPC": sopc, "ENC_SOP1": sop1, "ENC_VOP2": vop2,
            "ENC_VOPC": vopc, "ENC_VOP3": vop3,
        },
        "variants": variants,
    }


def _form(name, opcode, variant, operation, signedness, scc, carry_in="none"):
    return {
        "name": name, "family": "SOP2", "opcode": opcode, "variant": variant,
        "operation": operation, "signedness": signedness, "carry_in": carry_in,
        "width": 32, "scc": scc, "vcc": "unchanged", "exec": "unchanged", "wave": "any",
        "operands": "sop2_sgpr",
        "spec": [{"document": "guide", "section": "12.1", "page": 101}],
        "verify": ["reference", "independent"],
    }


def unaries():
    reg = registry()
    rule = copy.deepcopy(gen.OPERAND_RULES["sop1_sgpr"]["shape"])
    rule["unsupported"] = sorted(gen.OPERAND_RULES["sop1_sgpr"]["unsupported"])
    reg["operand_rules"]["sop1_sgpr"] = rule

    def unary(name, opcode, variant, operation, signedness, scc, **extra):
        form = _form(name, opcode, variant, operation, signedness, scc)
        form.update(family="SOP1", operands="sop1_sgpr", **extra)
        return form

    reg["form"] += [
        unary("S_NOT_B32", 7, "NotB32", "not", "bitwise", "nonzero"),
        unary("S_BREV_B32", 11, "BrevB32", "brev", "bitwise", "unchanged"),
        unary("S_SEXT_I32_I8", 25, "SextI32I8", "sext", "signed", "unchanged", source_bits=8),
    ]
    return reg


def saveexec():
    """A registry with the ten SAVEEXEC forms' rule and one of them."""
    reg = unaries()
    rule = copy.deepcopy(gen.OPERAND_RULES["sop1_saveexec"]["shape"])
    rule["unsupported"] = sorted(gen.OPERAND_RULES["sop1_saveexec"]["unsupported"])
    reg["operand_rules"]["sop1_saveexec"] = rule
    form = _form("S_AND_SAVEEXEC_B32", 60, "AndSaveExecB32", "and", "bitwise", "nonzero")
    form.update(family="SOP1", operands="sop1_saveexec", exec="exec_save")
    reg["form"].append(form)
    return reg


def ariths():
    reg = registry()
    reg["form"] += [
        _form("S_MIN_I32", 6, "MinI32", "min", "signed", "selected_src0"),
        _form("S_CSELECT_B32", 10, "CselectB32", "cselect", "bitwise", "unchanged", carry_in="scc"),
        _form("S_MUL_I32", 38, "MulI32", "mul", "signed", "unchanged"),
        _form("S_BFE_I32", 40, "BfeI32", "bfe", "signed", "nonzero"),
        dict(
            _form("S_LSHL2_ADD_U32", 47, "Lshl2AddU32", "lshl_add", "unsigned", "carry_out"),
            shift_amount=2,
        ),
        _form("S_MUL_HI_U32", 53, "MulHiU32", "mul_hi", "unsigned", "unchanged"),
    ]
    return reg


def vectors():
    reg = registry()
    rule = copy.deepcopy(gen.OPERAND_RULES["vop2_vgpr"]["shape"])
    rule["unsupported"] = sorted(gen.OPERAND_RULES["vop2_vgpr"]["unsupported"])
    reg["operand_rules"]["vop2_vgpr"] = rule

    def vector(name, opcode, variant, operation, signedness, **extra):
        form = _form(name, opcode, variant, operation, signedness, "unchanged")
        form.update(family="VOP2", operands="vop2_vgpr", **extra)
        return form

    reg["form"] += [
        vector("V_MUL_I32_I24", 9, "MulI32I24", "mul24", "signed"),
        # The same variant name as a scalar form: variants are per enum.
        vector("V_MIN_I32", 17, "AddU32", "min", "signed"),
        vector("V_SUBREV_NC_U32", 39, "SubrevNcU32", "sub", "unsigned", source_order="reversed"),
    ]
    return reg


def vector_compares():
    reg = registry()
    rule = copy.deepcopy(gen.OPERAND_RULES["vopc_vcc"]["shape"])
    rule["unsupported"] = sorted(gen.OPERAND_RULES["vopc_vcc"]["unsupported"])
    reg["operand_rules"]["vopc_vcc"] = rule

    def compare(name, opcode, variant, relation, signedness):
        form = _form(name, opcode, variant, "compare", signedness, "unchanged")
        form.update(
            family="VOPC", relation=relation, operands="vopc_vcc", vcc="lane_mask", wave="wave32"
        )
        return form

    reg["form"] += [
        # The same variant name as a SOPC form: variants are per enum.
        compare("V_CMP_LT_I32", 129, "CmpLtI32", "lt", "signed"),
        compare("V_CMP_T_I32", 135, "CmpTI32", "t", "signed"),
        compare("V_CMP_F_U32", 192, "CmpFU32", "f", "unsigned"),
        compare("V_CMP_NE_U32", 197, "CmpNeU32", "lg", "unsigned"),
    ]
    return reg


def variants():
    reg = vectors()
    rule = copy.deepcopy(gen.OPERAND_RULES["vop3_vgpr2"]["shape"])
    rule["unsupported"] = sorted(gen.OPERAND_RULES["vop3_vgpr2"]["unsupported"])
    reg["operand_rules"]["vop3_vgpr2"] = rule
    reg["encoding_variant"] = [
        _variant("V_MIN_I32", 273), _variant("V_SUBREV_NC_U32", 295),
    ]
    return reg


def _variant(of, opcode):
    return {
        "of": of, "family": "VOP3", "opcode": opcode, "operands": "vop3_vgpr2",
        "spec": [{"document": "guide", "section": "12.12", "page": 167}],
        "verify": ["reference", "independent"],
    }


def compare_variants():
    reg = vector_compares()
    rule = gen.OPERAND_RULES["vop3_cmp_vgpr2"]
    reg["operand_rules"]["vop3_cmp_vgpr2"] = dict(
        copy.deepcopy(rule["shape"]), unsupported=sorted(rule["unsupported"])
    )
    reg["encoding_variant"] = [
        dict(_variant("V_CMP_LT_I32", 129), operands="vop3_cmp_vgpr2")
    ]
    return reg


def compares():
    reg = registry()
    reg["form"] += [
        _compare("S_CMP_LT_I32", 4, "CmpLtI32", "lt", "signed"),
        _compare("S_CMP_LT_U32", 10, "CmpLtU32", "lt", "unsigned"),
    ]
    return reg


def _compare(name, opcode, variant, relation, signedness):
    form = _form(name, opcode, variant, "compare", signedness, "relation")
    form.update(family="SOPC", relation=relation, operands="sopc_scalar")
    return form


def registry():
    rule = copy.deepcopy(gen.OPERAND_RULES["sop2_sgpr"]["shape"])
    rule["unsupported"] = sorted(gen.UNSUPPORTED_COMBOS)
    compare_rule = copy.deepcopy(gen.OPERAND_RULES["sopc_scalar"]["shape"])
    compare_rule["unsupported"] = sorted(gen.OPERAND_RULES["sopc_scalar"]["unsupported"])
    return {
        "schema": 1,
        "architecture": "gfx1030",
        "source_sha256": SHA,
        "documents": copy.deepcopy(gen.PINNED_DOCUMENTS),
        "operand_rules": {"sop2_sgpr": rule, "sopc_scalar": compare_rule},
        "form": [
            _form("S_ADD_U32", 0, "AddU32", "add", "unsigned", "carry_out"),
            _form("S_ADD_I32", 2, "AddI32", "add", "signed", "signed_overflow"),
            _form("S_ADDC_U32", 4, "AddcU32", "add", "unsigned", "carry_out", carry_in="scc"),
            _form("S_AND_B32", 14, "AndB32", "and", "bitwise", "nonzero"),
            dict(
                _form("S_ASHR_I32", 34, "AshrI32", "ashr", "signed", "nonzero"),
                shift_count_bits=5,
            ),
        ],
    }


def form(reg, name):
    return next(item for item in reg["form"] if item["name"] == name)


class Validate(unittest.TestCase):
    def errors(self, reg, snap=None):
        return gen.validate(reg, snap if snap is not None else snapshot())

    def rejects(self, reg, fragment, snap=None):
        errors = self.errors(reg, snap)
        self.assertTrue(
            any(fragment in error for error in errors),
            f"expected an error containing {fragment!r}, got {errors}",
        )

    # ---- acceptance ----

    def test_accepts_the_fixture(self):
        self.assertEqual(self.errors(registry()), [])

    def test_accepts_the_real_registry(self):
        self.assertEqual(gen.validate(gen.load_registry(), gen.load_snapshot()), [])

    def test_a_reversed_shift_takes_its_count_from_src0(self):
        # V_ASHRREV_I32: D = S1 >> S0[4:0]; the XML gives SRC0 FMT_NUM_U32 (the
        # count) and VSRC1 FMT_NUM_I32. Without the declared reversed order the
        # count check lands on VSRC1 and must fail.
        reg = gen.load_registry()
        form = next(f for f in reg["form"] if f["name"] == "V_ASHRREV_I32")
        del form["source_order"]
        errors = gen.validate(reg, gen.load_snapshot())
        self.assertIn(
            "V_ASHRREV_I32: control operand VSRC1 has XML format FMT_NUM_I32, not FMT_NUM_U32",
            errors,
        )

    def test_rules_sharing_an_enum_render_it_once_with_each_forms_rule(self):
        text = gen.render(gen.load_registry(), gen.load_snapshot())
        self.assertEqual(text.count("pub enum VectorAluOp {"), 1)
        self.assertIn("Self::AndB32 => OperandRule::Vop2Vgpr2,", text)
        self.assertIn("Self::MinI32 => OperandRule::Vop2Vgpr,", text)

    # ---- top level ----

    def test_rejects_wrong_schema(self):
        reg = registry()
        reg["schema"] = 2
        self.rejects(reg, "schema 2 is not 1")

    def test_rejects_source_sha256_mismatch(self):
        reg = registry()
        reg["source_sha256"] = "f" * 64
        self.rejects(reg, "source_sha256 does not match")

    def test_rejects_unknown_top_level_key(self):
        reg = registry()
        reg["notes"] = "x"
        self.rejects(reg, "unknown top-level keys ['notes']")

    def test_rejects_empty_registry(self):
        reg = registry()
        reg["form"] = []
        self.rejects(reg, "declares no forms")

    # ---- incomplete or unknown fields ----

    def test_rejects_missing_required_field(self):
        reg = registry()
        del form(reg, "S_ADD_U32")["scc"]
        self.rejects(reg, "S_ADD_U32: incomplete, missing ['scc']")

    def test_rejects_unknown_form_key(self):
        reg = registry()
        form(reg, "S_ADD_U32")["expression"] = "a + b"
        self.rejects(reg, "S_ADD_U32: unknown keys ['expression']")

    # ---- closed vocabularies ----

    def test_rejects_unknown_operation(self):
        reg = registry()
        form(reg, "S_ADD_U32")["operation"] = "fma"
        self.rejects(reg, "S_ADD_U32: unknown operation 'fma'")

    def test_rejects_unknown_scc_effect(self):
        reg = registry()
        form(reg, "S_ADD_U32")["scc"] = "overflow"
        self.rejects(reg, "S_ADD_U32: unknown scc 'overflow'")

    def test_rejects_unknown_mask_effect(self):
        reg = registry()
        form(reg, "S_ADD_U32")["vcc"] = "written"
        self.rejects(reg, "S_ADD_U32: unknown vcc 'written'")

    def test_rejects_unsupported_width(self):
        reg = registry()
        form(reg, "S_ADD_U32")["width"] = 64
        self.rejects(reg, "S_ADD_U32: width 64 is not supported")

    def test_rejects_unknown_evidence(self):
        reg = registry()
        form(reg, "S_ADD_U32")["verify"] = ["reference", "hardware"]
        self.rejects(reg, "S_ADD_U32: unknown verification requirement")

    def test_rejects_empty_evidence(self):
        reg = registry()
        form(reg, "S_ADD_U32")["verify"] = []
        self.rejects(reg, "S_ADD_U32: no verification requirement")

    # ---- contradictions ----

    def test_rejects_bitwise_operation_declared_signed(self):
        reg = registry()
        form(reg, "S_AND_B32")["signedness"] = "signed"
        self.rejects(reg, "S_AND_B32: signedness signed contradicts operation and")

    def test_rejects_carry_in_on_logical_operation(self):
        reg = registry()
        form(reg, "S_AND_B32")["carry_in"] = "scc"
        self.rejects(reg, "S_AND_B32: carry_in scc contradicts and/bitwise")

    def test_rejects_carry_out_on_signed_form(self):
        reg = registry()
        form(reg, "S_ADD_I32")["scc"] = "carry_out"
        self.rejects(reg, "S_ADD_I32: scc carry_out contradicts add/signed")

    def test_rejects_scc_unchanged_on_a_form_that_writes_it(self):
        # Declared as preserving SCC while the primitive and the XML both write it.
        reg = registry()
        form(reg, "S_ADD_U32")["scc"] = "unchanged"
        self.rejects(reg, "S_ADD_U32: scc unchanged contradicts add/unsigned")
        self.rejects(reg, "S_ADD_U32: scc unchanged contradicts the XML SCC output")

    def test_rejects_carry_in_the_xml_does_not_read(self):
        reg = registry()
        form(reg, "S_ADD_U32")["carry_in"] = "scc"
        self.rejects(reg, "S_ADD_U32: carry_in scc contradicts the XML SCC input")

    def test_rejects_signedness_contradicting_xml_format(self):
        reg = registry()
        f = form(reg, "S_ADD_I32")
        f["signedness"], f["scc"] = "unsigned", "carry_out"
        self.rejects(reg, "S_ADD_I32: signedness unsigned contradicts XML format FMT_NUM_I32")

    # ---- identity against the XML snapshot ----

    def test_rejects_opcode_mismatch(self):
        reg = registry()
        form(reg, "S_ADD_U32")["opcode"] = 1
        self.rejects(reg, "S_ADD_U32: opcode 1 disagrees with the XML opcode 0")

    def test_rejects_family_mismatch(self):
        reg = registry()
        form(reg, "S_ADD_U32")["family"] = "SOPC"
        self.rejects(reg, "S_ADD_U32: operand rule sop2_sgpr is for SOP2")
        self.rejects(reg, "S_ADD_U32: no default ENC_SOPC variant in the XML snapshot")
        form(reg, "S_ADD_U32")["family"] = "SOPK"
        self.rejects(reg, "S_ADD_U32: family SOPK has no XML encoding ENC_SOPK")

    def test_rejects_form_absent_from_the_xml(self):
        reg = registry()
        reg["form"].append(_form("S_ABSDIFF_I32", 44, "AbsdiffI32", "absdiff", "signed", "nonzero"))
        self.rejects(reg, "S_ABSDIFF_I32: no default ENC_SOP2 variant in the XML snapshot")

    # ---- duplicates ----

    def test_rejects_duplicate_name(self):
        reg = registry()
        reg["form"].append(copy.deepcopy(form(reg, "S_ADD_U32")))
        self.rejects(reg, "S_ADD_U32: duplicate name S_ADD_U32")

    def test_rejects_same_encoding_claimed_twice(self):
        reg = registry()
        twin = copy.deepcopy(form(reg, "S_ADD_U32"))
        twin["name"], twin["variant"] = "S_ADD_U32_COPY", "AddU32Copy"
        reg["form"].append(twin)
        self.rejects(reg, "S_ADD_U32_COPY: duplicate opcode ('SOP2', 0)")

    def test_rejects_duplicate_variant(self):
        reg = registry()
        form(reg, "S_ADD_I32")["variant"] = "AddU32"
        self.rejects(reg, "S_ADD_I32: duplicate variant AddU32")

    # ---- references ----

    def test_rejects_missing_guide_reference(self):
        reg = registry()
        form(reg, "S_ADD_U32")["spec"] = [{"document": "xml", "section": "1", "page": 0}]
        self.rejects(reg, "S_ADD_U32: missing a guide section and page reference")

    def test_rejects_undeclared_document(self):
        reg = registry()
        form(reg, "S_ADD_U32")["spec"].append({"document": "blog", "section": "1", "page": 1})
        self.rejects(reg, "S_ADD_U32: spec cites undeclared document 'blog'")

    # ---- operand rules ----

    def test_rejects_unknown_operand_rule_on_a_form(self):
        reg = registry()
        form(reg, "S_ADD_U32")["operands"] = "vop2_vgpr"
        self.rejects(reg, "S_ADD_U32: unknown operand rule 'vop2_vgpr'")

    def test_rejects_operand_rule_the_decoder_does_not_implement(self):
        reg = registry()
        reg["operand_rules"]["vop3_vgpr"] = {"family": "VOP3"}
        self.rejects(reg, "operand rule vop3_vgpr: not implemented by the decoder")

    def test_rejects_operand_rule_shape_differing_from_the_decoder(self):
        reg = registry()
        reg["operand_rules"]["sop2_sgpr"]["max_literals"] = 2
        self.rejects(reg, "operand rule sop2_sgpr: declaration differs")

    def test_rejects_a_shift_without_a_count_width(self):
        reg = registry()
        del reg["form"][-1]["shift_count_bits"]
        self.rejects(reg, "S_ASHR_I32: shift_count_bits must be log2 of the width (32), got None")

    def test_rejects_a_wrong_count_width(self):
        reg = registry()
        reg["form"][-1]["shift_count_bits"] = 6
        self.rejects(reg, "S_ASHR_I32: shift_count_bits must be log2 of the width (32), got 6")

    def test_rejects_a_count_width_on_a_non_shift(self):
        reg = registry()
        reg["form"][0]["shift_count_bits"] = 5
        self.rejects(reg, "S_ADD_U32: shift_count_bits on a non-shift operation")

    def test_rejects_a_signed_shift_count_in_the_xml(self):
        snap = snapshot()
        snap["variants"][("S_ASHR_I32", "ENC_SOP2")]["operands"] = _operands(I32)
        self.rejects(registry(), "S_ASHR_I32: control operand SSRC1 has XML format FMT_NUM_I32", snap)

    def test_rejects_a_float_shift_count_in_the_xml(self):
        snap = snapshot()
        snap["variants"][("S_ASHR_I32", "ENC_SOP2")]["operands"] = _operands(
            I32, src1_format="FMT_NUM_F32"
        )
        self.rejects(registry(), "S_ASHR_I32: control operand SSRC1 has XML format FMT_NUM_F32", snap)

    def test_rejects_a_float_source_format(self):
        snap = snapshot()
        snap["variants"][("S_AND_B32", "ENC_SOP2")]["operands"] = _operands("FMT_NUM_F32")
        self.rejects(registry(), "S_AND_B32: signedness bitwise contradicts XML format FMT_NUM_F32", snap)

    def test_rejects_a_negative_count_width(self):
        reg = registry()
        reg["form"][-1]["shift_count_bits"] = -1
        self.rejects(reg, "S_ASHR_I32: shift_count_bits must be log2 of the width (32), got -1")

    def test_rejects_an_unsigned_arithmetic_shift(self):
        reg = registry()
        reg["form"][-1]["signedness"] = "unsigned"
        self.rejects(reg, "S_ASHR_I32: no primitive implements ashr/unsigned")

    def test_renders_the_declared_count_mask(self):
        self.assertIn(
            "let (value, scc) = primitive::ashr_nonzero_i32(src0, src1 & 0x1f);",
            gen.render(registry(), snapshot()),
        )

    def test_accepts_unaries(self):
        self.assertEqual(self.errors(unaries()), [])

    def test_rejects_a_sext_without_source_bits(self):
        reg = unaries()
        del reg["form"][-1]["source_bits"]
        self.rejects(reg, "S_SEXT_I32_I8: source_bits must be one of [8, 16], got None")

    def test_rejects_a_sext_whose_source_bits_contradict_the_xml(self):
        reg = unaries()
        reg["form"][-1]["source_bits"] = 16
        self.rejects(reg, "S_SEXT_I32_I8: SSRC0 is 8 bits in the XML, declared 16")
        self.rejects(reg, "S_SEXT_I32_I8: SSRC0 has XML format FMT_NUM_I8, not FMT_NUM_I16")

    def test_rejects_source_bits_on_another_operation(self):
        reg = unaries()
        reg["form"][-2]["source_bits"] = 8
        self.rejects(reg, "S_BREV_B32: source_bits on a non-sext operation")

    def test_rejects_an_scc_write_the_xml_does_not_show(self):
        reg = unaries()
        reg["form"][-2]["operation"] = "not"
        reg["form"][-2]["scc"] = "nonzero"
        self.rejects(reg, "S_BREV_B32: scc nonzero contradicts the XML SCC output")

    def test_rejects_a_unary_operation_under_a_binary_rule(self):
        reg = unaries()
        reg["form"][0]["operation"] = "not"
        reg["form"][0]["scc"] = "nonzero"
        self.rejects(reg, "S_ADD_U32: operation not does not fit rule sop2_sgpr")

    def test_renders_unary_scc_as_optional(self):
        text = gen.render(unaries(), snapshot())
        self.assertIn("Self::BrevB32 => (primitive::brev_b32(src), exec, None),", text)
        self.assertIn("Self::SextI32I8 => (primitive::sext_i32(src, 8), exec, None),", text)
        self.assertIn("let (value, scc) = primitive::not_nonzero_b32(src);", text)

    def test_accepts_ariths(self):
        self.assertEqual(self.errors(ariths()), [])

    def test_rejects_min_with_a_nonzero_scc(self):
        reg = ariths()
        form(reg, "S_MIN_I32")["scc"] = "nonzero"
        self.rejects(reg, "S_MIN_I32: scc nonzero contradicts min/signed (requires selected_src0)")

    def test_rejects_a_select_that_does_not_read_scc(self):
        reg = ariths()
        form(reg, "S_CSELECT_B32")["carry_in"] = "none"
        self.rejects(reg, "S_CSELECT_B32: operation cselect reads SCC (requires carry_in scc)")
        self.rejects(reg, "S_CSELECT_B32: carry_in none contradicts the XML SCC input")

    def test_rejects_a_select_that_writes_scc(self):
        reg = ariths()
        form(reg, "S_CSELECT_B32")["scc"] = "nonzero"
        self.rejects(reg, "S_CSELECT_B32: scc nonzero contradicts cselect/bitwise (requires unchanged)")
        self.rejects(reg, "S_CSELECT_B32: scc nonzero contradicts the XML SCC output")

    def test_rejects_a_typed_select(self):
        reg = ariths()
        form(reg, "S_CSELECT_B32")["signedness"] = "unsigned"
        self.rejects(reg, "S_CSELECT_B32: signedness unsigned contradicts operation cselect")

    def test_rejects_a_multiply_that_writes_scc(self):
        reg = ariths()
        form(reg, "S_MUL_I32")["scc"] = "signed_overflow"
        self.rejects(reg, "S_MUL_I32: scc signed_overflow contradicts mul/signed (requires unchanged)")

    def test_rejects_a_bitwise_multiply(self):
        reg = ariths()
        form(reg, "S_MUL_HI_U32")["signedness"] = "bitwise"
        self.rejects(reg, "S_MUL_HI_U32: signedness bitwise contradicts operation mul_hi")

    def test_rejects_a_signed_bfe_descriptor_in_the_xml(self):
        snap = snapshot()
        snap["variants"][("S_BFE_I32", "ENC_SOP2")]["operands"] = _operands(I32)
        self.rejects(ariths(), "S_BFE_I32: control operand SSRC1 has XML format FMT_NUM_I32", snap)

    def test_rejects_a_signed_bfe_data_operand_in_the_xml(self):
        snap = snapshot()
        snap["variants"][("S_BFE_I32", "ENC_SOP2")]["operands"] = _operands(U32)
        self.rejects(ariths(), "S_BFE_I32: signedness signed contradicts XML format FMT_NUM_U32", snap)

    def test_rejects_shift_add_without_or_with_a_wrong_amount(self):
        for amount, shown in ((None, "None"), (0, "0"), (5, "5"), (True, "True"), ("2", "'2'")):
            with self.subTest(amount=amount):
                reg = ariths()
                if amount is None:
                    del form(reg, "S_LSHL2_ADD_U32")["shift_amount"]
                else:
                    form(reg, "S_LSHL2_ADD_U32")["shift_amount"] = amount
                self.rejects(reg, f"S_LSHL2_ADD_U32: shift_amount must be one of [1, 2, 3, 4], got {shown}")

    def test_rejects_a_shift_amount_on_another_operation(self):
        reg = ariths()
        form(reg, "S_MIN_I32")["shift_amount"] = 1
        self.rejects(reg, "S_MIN_I32: shift_amount on a non-lshl_add operation")

    def test_renders_alu_scc_as_optional_and_the_declared_data(self):
        text = gen.render(ariths(), snapshot())
        self.assertIn("Self::CselectB32 => (primitive::cselect_b32(src0, src1, scc_in), None),", text)
        self.assertIn("Self::MulHiU32 => (primitive::mul_hi_u32(src0, src1), None),", text)
        self.assertIn("let (value, scc) = primitive::lshl_add_carry_u32(src0, src1, 2);", text)
        self.assertIn("let (value, scc) = primitive::min_i32(src0, src1);", text)
        self.assertIn("Self::CselectB32 => true,", text.split("fn reads_scc", 1)[1].split("}", 1)[0])
        self.assertIn("Self::CselectB32 => false,", text.split("fn writes_scc", 1)[1].split("}", 1)[0])
        self.assertIn("shift_amount: 2,", text)
        self.assertIn("scc: SccEffect::SelectedSrc0,", text)

    def test_accepts_vectors(self):
        self.assertEqual(self.errors(vectors()), [])

    def test_rejects_a_vector_form_that_writes_scc(self):
        reg = vectors()
        form(reg, "V_MIN_I32")["scc"] = "selected_src0"
        self.rejects(reg, "V_MIN_I32: scc selected_src0 contradicts min/signed (requires unchanged)")

    def test_rejects_a_scalar_only_operation_under_the_vector_rule(self):
        reg = vectors()
        form(reg, "V_MUL_I32_I24")["operation"] = "mul"
        self.rejects(reg, "V_MUL_I32_I24: no primitive implements mul/signed")

    def test_rejects_a_vector_only_operation_under_a_scalar_rule(self):
        reg = registry()
        form(reg, "S_ADD_I32")["operation"] = "mul24"
        self.rejects(reg, "S_ADD_I32: no primitive implements mul24/signed")

    def test_rejects_a_bitwise_24_bit_multiply(self):
        reg = vectors()
        form(reg, "V_MUL_I32_I24")["signedness"] = "bitwise"
        self.rejects(reg, "V_MUL_I32_I24: signedness bitwise contradicts operation mul24")

    def test_rejects_a_source_order_on_a_scalar_form(self):
        reg = vectors()
        form(reg, "S_ADD_U32")["source_order"] = "reversed"
        self.rejects(reg, "S_ADD_U32: source_order on a non-vector form")

    def test_rejects_an_unknown_source_order(self):
        for order in ("normal", True, ["reversed"]):
            with self.subTest(order=order):
                reg = vectors()
                form(reg, "V_SUBREV_NC_U32")["source_order"] = order
                self.rejects(reg, f"V_SUBREV_NC_U32: unknown source_order {order!r}")

    def test_rejects_a_duplicate_variant_within_one_enum(self):
        reg = vectors()
        form(reg, "V_SUBREV_NC_U32")["variant"] = "MulI32I24"
        self.rejects(reg, "V_SUBREV_NC_U32: duplicate variant MulI32I24")

    def test_rejects_a_vector_rule_without_its_null_rejection(self):
        reg = vectors()
        reg["operand_rules"]["vop2_vgpr"]["unsupported"].remove("src_null")
        self.rejects(reg, "operand rule vop2_vgpr: unsupported combos differ from those the decoder rejects")

    def test_rejects_an_implicit_vcc_operand_in_the_xml(self):
        snap = snapshot()
        snap["variants"][("V_MIN_I32", "ENC_VOP2")]["operands"].append(
            ["", "FMT_NUM_B32", "OPR_VCC", 32, True, False, True, False]
        )
        self.rejects(vectors(), "V_MIN_I32: XML implicit operands ['OPR_VCC'] are not declared", snap)

    def test_renders_vector_lowering_and_source_order(self):
        text = gen.render(vectors(), snapshot())
        self.assertIn("pub enum VectorAluOp {", text)
        self.assertIn("if word & 0x8000_0000 != 0x0000_0000 {", text)
        self.assertIn("Self::MulI32I24 => VectorLowering::Binary(VectorBinary::Mul24Signed),", text)
        self.assertIn("Self::SubrevNcU32 => VectorLowering::Binary(VectorBinary::Sub),", text)
        self.assertIn("Self::SubrevNcU32 => true,", text.split("fn swaps_sources", 1)[1])
        self.assertIn('Self::AddU32 => "v_min_i32_e32",', text)
        self.assertIn("swap_sources: true,", text)

    def test_vop2_vcc_requires_the_xml_input_and_output(self):
        reg = gen.load_registry()
        snap = gen.load_snapshot()
        operands = snap["variants"][("V_ADD_CO_CI_U32", "ENC_VOP2")]["operands"]
        operands[:] = [op for op in operands if not (op[2] == "OPR_VCC" and op[4])]
        self.rejects(reg, "V_ADD_CO_CI_U32: carry_in vcc requires one implicit XML VCC input", snap)
        form(reg, "V_CNDMASK_B32")["vcc"] = "lane_mask"
        self.rejects(reg, "V_CNDMASK_B32: vcc lane_mask contradicts rule vop2_vcc", snap)

    def test_accepts_compares(self):
        self.assertEqual(self.errors(compares()), [])

    def test_rejects_a_compare_without_a_relation(self):
        reg = compares()
        del reg["form"][-1]["relation"]
        self.rejects(reg, "S_CMP_LT_U32: unknown or missing relation None")

    def test_rejects_an_unknown_relation(self):
        reg = compares()
        reg["form"][-1]["relation"] = "ne"
        self.rejects(reg, "S_CMP_LT_U32: unknown or missing relation 'ne'")

    def test_rejects_a_non_string_relation(self):
        reg = compares()
        reg["form"][-1]["relation"] = ["lt"]
        self.rejects(reg, "S_CMP_LT_U32: unknown or missing relation ['lt']")

    def test_rejects_a_rule_without_sources(self):
        reg = compares()
        del reg["operand_rules"]["sopc_scalar"]["src"]
        self.rejects(reg, "operand rule sopc_scalar: declaration differs from the decoder's implemented rule")

    def test_rejects_a_relation_on_a_non_compare(self):
        reg = compares()
        reg["form"][0]["relation"] = "lt"
        self.rejects(reg, "S_ADD_U32: relation on a non-compare operation")

    def test_rejects_a_compare_with_another_scc_effect(self):
        reg = compares()
        reg["form"][-1]["scc"] = "nonzero"
        self.rejects(reg, "S_CMP_LT_U32: scc nonzero contradicts compare (requires relation)")

    def test_rejects_a_bitwise_compare(self):
        reg = compares()
        reg["form"][-1]["signedness"] = "bitwise"
        self.rejects(reg, "S_CMP_LT_U32: no primitive implements lt/bitwise")

    def test_rejects_a_compare_whose_signedness_contradicts_the_xml(self):
        reg = compares()
        reg["form"][-1]["signedness"] = "signed"
        self.rejects(reg, "S_CMP_LT_U32: signedness signed contradicts XML format FMT_NUM_U32")

    def test_rejects_a_compare_under_an_alu_rule(self):
        reg = compares()
        reg["form"][0]["operation"] = "compare"
        reg["form"][0]["relation"] = "lt"
        reg["form"][0]["scc"] = "relation"
        self.rejects(reg, "S_ADD_U32: operation compare does not fit rule sop2_sgpr")

    def test_renders_compare_dispatch_and_relation(self):
        text = gen.render(compares(), snapshot())
        self.assertIn("if word & 0xff80_0000 != 0xbf00_0000 {", text)
        self.assertIn("Self::CmpLtI32 => primitive::lt_i32(src0, src1),", text)
        self.assertIn("Self::CmpLtU32 => primitive::lt_u32(src0, src1),", text)
        self.assertIn("relation: Some(CompareRelation::Less),", text)

    def test_rejects_an_incomplete_unsupported_list(self):
        reg = registry()
        reg["operand_rules"]["sop2_sgpr"]["unsupported"].remove("two_literals")
        self.rejects(
            reg, "operand rule sop2_sgpr: unsupported combos differ from those the decoder rejects"
        )

    def test_rejects_a_citation_to_another_file(self):
        reg = registry()
        reg["documents"]["guide"]["file"] = "missing.pdf"
        self.rejects(reg, "document guide: differs from the pinned public document")

    # ---- encoding variants ----

    def test_accepts_encoding_variants(self):
        self.assertEqual(self.errors(variants()), [])

    def test_compare_variant_routes_mask_to_sdst(self):
        reg = compare_variants()
        self.assertEqual(self.errors(reg), [])
        row = gen.variant_row(reg, reg["encoding_variant"][0])
        self.assertEqual((row["vcc"], row["width"], row["wave"]), ("sdst_lane_mask", 32, "wave32"))

    def test_rejects_mismatched_compare_variant_destination(self):
        reg = compare_variants()
        form(reg, "V_CMP_LT_I32")["vcc"] = "unchanged"
        self.rejects(reg, "requires a base destination contract of lane_mask, got unchanged")

        reg = compare_variants()
        reg["operand_rules"]["vop3_cmp_vgpr2"]["dst"]["class"] = "vgpr"
        self.rejects(reg, "operand rule vop3_cmp_vgpr2: declaration differs")

        snap = snapshot()
        snap["variants"][("V_CMP_LT_I32", "ENC_VOP3")]["operands"][0][2] = "OPR_VGPR"
        self.rejects(compare_variants(), "VDST is not a scalar mask destination in the XML", snap)

    def test_rejects_a_variant_of_an_undeclared_form(self):
        reg = variants()
        reg["encoding_variant"].append(_variant("V_MUL_HI_I32_I24", 266))
        self.rejects(reg, "V_MUL_HI_I32_I24 (VOP3): encoding variant of an undeclared form")

    def test_rejects_an_incomplete_or_unknown_variant_field(self):
        reg = variants()
        del reg["encoding_variant"][0]["spec"]
        reg["encoding_variant"][1]["operation"] = "sub"
        self.rejects(reg, "V_MIN_I32 (VOP3): incomplete encoding variant, missing ['spec']")
        self.rejects(reg, "V_SUBREV_NC_U32 (VOP3): unknown keys ['operation']")

    def test_rejects_a_variant_opcode_that_disagrees_with_the_xml(self):
        reg = variants()
        reg["encoding_variant"][0]["opcode"] = 265
        self.rejects(reg, "V_MIN_I32: opcode 265 disagrees with the XML opcode 273")

    def test_rejects_a_duplicate_variant_or_opcode(self):
        reg = variants()
        reg["encoding_variant"].append(_variant("V_MIN_I32", 273))
        self.rejects(reg, "V_MIN_I32 (VOP3): duplicate encoding of V_MIN_I32")
        self.rejects(reg, "V_MIN_I32 (VOP3): duplicate opcode ('VOP3', 273)")

    def test_rejects_a_variant_in_its_forms_own_family(self):
        reg = variants()
        reg["encoding_variant"][0].update(family="VOP2", opcode=17, operands="vop2_vgpr")
        self.rejects(reg, "V_MIN_I32 (VOP2): an encoding variant must use another family than its form")

    def test_rejects_a_variant_rule_that_cannot_encode_the_form(self):
        reg = variants()
        reg["encoding_variant"][0]["operands"] = "sop2_sgpr"
        self.rejects(reg, "V_MIN_I32 (VOP3): operand rule sop2_sgpr is for SOP2")
        self.rejects(reg, "V_MIN_I32 (VOP3): operand rule sop2_sgpr cannot encode a vector form")

    def test_rejects_a_form_that_uses_a_variant_only_rule(self):
        reg = variants()
        form(reg, "V_MIN_I32")["operands"] = "vop3_vgpr2"
        self.rejects(reg, "V_MIN_I32: operand rule vop3_vgpr2 is only for encoding variants")

    def test_checks_the_variant_xml_against_the_inherited_declaration(self):
        snap = snapshot()
        for row in snap["variants"][("V_MIN_I32", "ENC_VOP3")]["operands"]:
            row[1] = U32
        self.rejects(variants(), "V_MIN_I32: signedness signed contradicts XML format FMT_NUM_U32", snap)

    def test_rejects_a_variant_destination_format_that_differs_from_the_form(self):
        snap = snapshot()
        snap["variants"][("V_MIN_I32", "ENC_VOP3")]["operands"][0][1] = "FMT_NUM_F32"
        self.rejects(
            variants(),
            "V_MIN_I32 (VOP3): XML VDST format FMT_NUM_F32 differs from the form's VDST format FMT_NUM_I32",
            snap,
        )

    def test_compares_variant_formats_by_role_not_row_order(self):
        snap = snapshot()
        # Give both encodings an unsigned destination and signed sources, then
        # list the VOP3 rows in another order: by role they still match.
        snap["variants"][("V_MIN_I32", "ENC_VOP2")]["operands"][0][1] = U32
        rows = snap["variants"][("V_MIN_I32", "ENC_VOP3")]["operands"]
        rows[0][1] = U32
        rows[0], rows[1] = rows[1], rows[0]
        self.assertEqual(self.errors(variants(), snap), [])
        # A genuine mismatch on the second source is still reported by role.
        rows[2][1] = U32
        self.rejects(
            variants(), "V_MIN_I32 (VOP3): XML SRC1 format FMT_NUM_U32 differs from the form's VSRC1", snap
        )

    def test_a_malformed_variant_rule_is_a_diagnostic(self):
        for key, value in (("src", 1), ("dst", {"field": [], "class": "vgpr", "min": 0, "max": 255})):
            with self.subTest(key=key):
                reg = variants()
                reg["operand_rules"]["vop3_vgpr2"][key] = value
                self.rejects(
                    reg, "operand rule vop3_vgpr2: declaration differs from the decoder's implemented rule"
                )

    def test_rejects_malformed_spec_and_verify_without_a_traceback(self):
        cases = (
            ("spec", 1, "spec must be a list of references"),
            ("spec", [[]], "spec reference needs exactly document, section, page"),
            ("spec", [{"document": [], "section": "1", "page": 1}], "spec document must be a name"),
            ("verify", 1, "invalid verification requirements"),
            ("verify", [[]], "invalid verification requirements"),
        )
        for key, value, fragment in cases:
            with self.subTest(key=key, value=value):
                reg = variants()
                reg["encoding_variant"][0][key] = value
                self.rejects(reg, fragment)
        for key, value, fragment in cases:
            with self.subTest(form=True, key=key, value=value):
                reg = variants()
                form(reg, "V_MIN_I32")[key] = value
                expected = "verify must be a list of evidence names" if key == "verify" else fragment
                self.rejects(reg, expected)

    def test_rejects_malformed_variant_fields_without_a_traceback(self):
        for key in ("of", "operands", "family"):
            with self.subTest(key=key):
                reg = variants()
                reg["encoding_variant"][0][key] = []
                self.rejects(reg, f"[{key!r}] must be strings")

    def test_a_variant_of_an_incomplete_form_reports_the_form_not_a_traceback(self):
        reg = variants()
        base = next(f for f in reg["form"] if f["name"] == reg["encoding_variant"][0]["of"])
        del base["width"]
        errors = self.errors(reg)
        self.assertTrue(any("missing ['width']" in error for error in errors), errors)
        self.assertTrue(any("is incomplete" in error for error in errors), errors)

    def test_a_variant_of_an_incomplete_first_duplicate_is_a_diagnostic(self):
        reg = variants()
        base = next(f for f in reg["form"] if f["name"] == reg["encoding_variant"][0]["of"])
        incomplete = copy.deepcopy(base)
        del incomplete["width"]
        reg["form"].insert(reg["form"].index(base), incomplete)
        errors = self.errors(reg)
        self.assertTrue(any("missing ['width']" in error for error in errors), errors)

    def test_a_non_array_or_non_table_variant_is_a_diagnostic(self):
        reg = variants()
        reg["encoding_variant"] = 1
        self.rejects(reg, "encoding_variant must be an array of tables")
        reg = variants()
        reg["encoding_variant"].append(1)
        self.rejects(reg, "must be a table, got 1")

    def test_renders_variant_dispatch_and_rows(self):
        text = gen.render(variants(), snapshot())
        self.assertIn("pub enum VectorAluVop3Op {", text)
        self.assertIn("pub const fn from_word(word: u32) -> Option<Self> {", text.split("pub enum VectorAluVop3Op", 1)[1])
        self.assertIn("if word & 0xfc00_0000 != 0xd400_0000 {", text)
        self.assertIn("273 => Some(Self::AddU32),", text)  # V_MIN_I32's variant name
        self.assertIn("Self::AddU32 => VectorAluOp::AddU32,", text)
        self.assertIn('Self::SubrevNcU32 => "v_subrev_nc_u32_e64",', text)
        self.assertIn('("V_MIN_I32", EncodingFamily::Vop3),', text)
        self.assertIn("operands: OperandRule::Vop3Vgpr2,", text)
        # The VOP3 row inherits SUBREV's reversed sources.
        row = text.split('name: "V_SUBREV_NC_U32",\n        family: EncodingFamily::Vop3,', 1)[1]
        self.assertIn("swap_sources: true,", row.split("}", 1)[0])

    def test_rejects_an_unenforced_wave_restriction(self):
        reg = registry()
        reg["form"][0]["wave"] = "wave32"
        self.rejects(reg, "S_ADD_U32: wave wave32 is not enforced for this rule (requires any)")

    # ---- VOPC lane-mask compares ----

    def test_accepts_vector_compares(self):
        self.assertEqual(self.errors(vector_compares()), [])

    def test_rejects_a_vector_compare_declared_for_any_wave(self):
        reg = vector_compares()
        form(reg, "V_CMP_LT_I32")["wave"] = "any"
        self.rejects(reg, "V_CMP_LT_I32: wave any is not enforced for this rule (requires wave32)")

    def test_rejects_a_vector_compare_without_its_vcc_write(self):
        reg = vector_compares()
        form(reg, "V_CMP_LT_I32")["vcc"] = "unchanged"
        self.rejects(reg, "V_CMP_LT_I32: vcc unchanged contradicts rule vopc_vcc (requires lane_mask)")
        # The XML VCC output is then undeclared as well.
        self.rejects(reg, "V_CMP_LT_I32: XML implicit operands ['OPR_VCC'] are not declared")

    def test_rejects_a_lane_mask_outside_vopc(self):
        reg = registry()
        reg["form"][0]["vcc"] = "lane_mask"
        self.rejects(reg, "S_ADD_U32: vcc lane_mask contradicts rule sop2_sgpr (requires unchanged)")
        self.rejects(reg, "S_ADD_U32: vcc lane_mask but the XML has no implicit VCC output")

    def test_rejects_an_exec_write(self):
        reg = vector_compares()
        form(reg, "V_CMP_LT_I32")["exec"] = "lane_mask"
        self.rejects(reg, "V_CMP_LT_I32: unknown exec 'lane_mask'")

    def test_accepts_an_exec_write_on_a_saveexec_form(self):
        reg = saveexec()
        self.assertEqual(self.errors(reg), [])
        text = gen.render(reg, snapshot())
        # The destination keeps the previous mask and the mask is updated.
        self.assertIn("Self::AndSaveExecB32 => {", text)
        self.assertIn("let (value, scc) = primitive::and_nonzero_b32(src, exec);", text)
        self.assertIn("(exec, value, Some(scc))", text)
        self.assertIn("exec: MaskEffect::ExecSave,", text)

    def test_rejects_an_exec_write_outside_the_sop1_contract(self):
        reg = saveexec()
        form(reg, "S_AND_SAVEEXEC_B32")["family"] = "VOP1"
        self.rejects(reg, "S_AND_SAVEEXEC_B32: exec exec_save is only defined for 32-bit SOP1 forms")

    def test_rejects_an_exec_write_without_the_saveexec_rule(self):
        reg = saveexec()
        form(reg, "S_AND_SAVEEXEC_B32")["operands"] = "sop1_sgpr"
        self.rejects(reg, "S_AND_SAVEEXEC_B32: exec exec_save requires the sop1_saveexec rule")

    def test_rejects_an_exec_write_with_an_unchanged_scc(self):
        reg = saveexec()
        form(reg, "S_AND_SAVEEXEC_B32")["scc"] = "unchanged"
        self.rejects(reg, "S_AND_SAVEEXEC_B32: exec exec_save requires the nonzero SCC effect")

    def test_rejects_an_exec_write_the_xml_does_not_show(self):
        reg = saveexec()
        snap = snapshot()
        rows = snap["variants"][("S_AND_SAVEEXEC_B32", "ENC_SOP1")]["operands"]
        snap["variants"][("S_AND_SAVEEXEC_B32", "ENC_SOP1")]["operands"] = [
            row for row in rows if row[2] != "OPR_SDST_EXEC"
        ]
        self.rejects(reg, "S_AND_SAVEEXEC_B32: exec exec_save needs one implicit EXEC read and one implicit EXEC write", snap)

    def test_rejects_a_vector_compare_that_writes_scc(self):
        reg = vector_compares()
        form(reg, "V_CMP_LT_I32")["scc"] = "relation"
        self.rejects(reg, "V_CMP_LT_I32: scc relation contradicts compare (requires unchanged)")

    def test_rejects_a_vector_compare_without_a_relation(self):
        reg = vector_compares()
        del form(reg, "V_CMP_NE_U32")["relation"]
        self.rejects(reg, "V_CMP_NE_U32: unknown or missing relation None")

    def test_rejects_a_vector_compare_under_a_scalar_rule(self):
        reg = vector_compares()
        form(reg, "V_CMP_LT_I32")["operands"] = "sopc_scalar"
        self.rejects(reg, "V_CMP_LT_I32: operand rule sopc_scalar is for SOPC")

    def test_rejects_a_vector_compare_signedness_against_the_xml(self):
        reg = vector_compares()
        form(reg, "V_CMP_LT_I32")["signedness"] = "unsigned"
        self.rejects(reg, "V_CMP_LT_I32: signedness unsigned contradicts XML format FMT_NUM_I32")

    def test_rejects_f_and_t_outside_vopc(self):
        for relation in ("f", "t"):
            with self.subTest(relation=relation):
                reg = compares()
                form(reg, "S_CMP_LT_I32")["relation"] = relation
                self.rejects(reg, f"S_CMP_LT_I32: relation {relation} exists only for VOPC compares")

    def test_renders_f_and_t_primitives(self):
        reg = vector_compares()
        self.assertEqual(self.errors(reg), [])
        text = gen.render(reg, snapshot())
        self.assertIn("Self::CmpTI32 => primitive::true_b32(src0, src1),", text)
        self.assertIn("Self::CmpFU32 => CompareRelation::False,", text)

    def test_rejects_a_vopc_relation_that_contradicts_the_opcode(self):
        for name, relation, expected in (
            ("V_CMP_T_I32", "f", 128), ("V_CMP_LT_I32", "gt", 132), ("V_CMP_F_U32", "t", 199),
        ):
            with self.subTest(name=name):
                reg = vector_compares()
                form(reg, name)["relation"] = relation
                signedness = form(reg, name)["signedness"]
                self.rejects(
                    reg,
                    f"{name}: relation {relation}/{signedness} has VOPC opcode {expected} "
                    f"(guide Table 59), not {form(reg, name)['opcode']}",
                )

    def test_rejects_an_implicit_vcc_read_as_a_lane_mask_write(self):
        for direction in ([True, False], [True, True]):
            with self.subTest(direction=direction):
                snap = snapshot()
                operands = snap["variants"][("V_CMP_T_I32", "ENC_VOPC")]["operands"]
                operands[0][4:6] = direction
                self.rejects(
                    vector_compares(),
                    "V_CMP_T_I32: vcc lane_mask but the XML has no implicit VCC output",
                    snap,
                )

    def test_rejects_a_second_implicit_vcc_operand(self):
        snap = snapshot()
        operands = snap["variants"][("V_CMP_T_I32", "ENC_VOPC")]["operands"]
        operands.append(["", "FMT_NUM_M64", "OPR_VCC", 64, True, False, False, False])
        self.rejects(
            vector_compares(), "V_CMP_T_I32: vcc lane_mask but the XML has no implicit VCC output", snap
        )

    def test_renders_vector_compare_dispatch(self):
        text = gen.render(vector_compares(), snapshot())
        self.assertIn("pub enum VectorCompareOp {", text)
        self.assertIn("if word & 0xfe00_0000 != 0x7c00_0000 {", text)
        self.assertIn('Self::CmpNeU32 => "v_cmp_ne_u32_e32",', text)
        self.assertIn("Self::CmpLtI32 => true,", text.split("fn signed", 1)[1])
        self.assertIn("Self::CmpNeU32 => primitive::ne_b32(src0, src1),", text)
        self.assertIn("vcc: MaskEffect::LaneMask,", text)
        self.assertIn("wave: Wave::Wave32,", text)

    def test_rejects_a_reserved_variant(self):
        reg = registry()
        reg["form"][0]["variant"] = "Self"
        self.rejects(reg, "S_ADD_U32: variant 'Self' is reserved")

    def test_rejects_a_boolean_schema(self):
        reg = registry()
        reg["schema"] = True
        self.rejects(reg, "schema True is not 1")

    def test_rejects_unknown_unsupported_combo(self):
        reg = registry()
        reg["operand_rules"]["sop2_sgpr"]["unsupported"].append("dst_lds")
        self.rejects(reg, "operand rule sop2_sgpr: unknown unsupported combos ['dst_lds']")

    def test_rejects_duplicate_unsupported_combo(self):
        reg = registry()
        reg["operand_rules"]["sop2_sgpr"]["unsupported"].append("dst_vcc")
        self.rejects(reg, "operand rule sop2_sgpr: duplicate unsupported combos")

    # ---- remaining vocabulary, identity and reference rules ----

    def test_rejects_wrong_architecture(self):
        reg = registry()
        reg["architecture"] = "gfx1100"
        self.rejects(reg, "architecture 'gfx1100' is not gfx1030")

    def test_rejects_unknown_document(self):
        reg = registry()
        reg["documents"]["blog"] = {"title": "blog"}
        self.rejects(reg, "unknown document blog")

    def test_rejects_unknown_signedness_carry_in_exec_and_wave(self):
        for key, value in (
            ("signedness", "bogus-signedness"), ("carry_in", "exec"),
            ("exec", "written"), ("wave", "wave64"),
        ):
            with self.subTest(key=key):
                reg = registry()
                form(reg, "S_ADD_U32")[key] = value
                self.rejects(reg, f"S_ADD_U32: unknown {key} {value!r}")

    def test_rejects_float_signedness_for_integer_add(self):
        reg = registry()
        form(reg, "S_ADD_U32")["signedness"] = "float"
        self.rejects(reg, "S_ADD_U32: signedness float contradicts operation add")

    def test_rejects_unsigned_signedness_for_float_rounding(self):
        reg = gen.load_registry()
        form(reg, "V_TRUNC_F32")["signedness"] = "unsigned"
        self.rejects(
            reg, "V_TRUNC_F32: signedness unsigned contradicts operation trunc", gen.load_snapshot()
        )

    def test_rejects_invalid_name_variant_and_opcode(self):
        for key, value, fragment in (
            ("name", "s_add_u32", "s_add_u32: invalid name"),
            ("variant", "add_u32", "S_ADD_U32: invalid variant 'add_u32'"),
            ("opcode", -1, "S_ADD_U32: invalid opcode"),
            ("opcode", True, "S_ADD_U32: invalid opcode"),
        ):
            with self.subTest(key=key, value=value):
                reg = registry()
                form(reg, "S_ADD_U32")[key] = value
                self.rejects(reg, fragment)

    def test_rejects_declaration_no_primitive_implements(self):
        reg = registry()
        form(reg, "S_AND_B32")["operation"] = "sub"
        self.rejects(reg, "S_AND_B32: no primitive implements sub/bitwise")

    def test_rejects_duplicate_evidence(self):
        reg = registry()
        form(reg, "S_ADD_U32")["verify"] = ["reference", "reference"]
        self.rejects(reg, "S_ADD_U32: duplicate verification requirement")

    def test_rejects_malformed_spec_references(self):
        for ref, fragment in (
            ({"document": "guide", "section": "12.1"}, "spec reference needs exactly document, section, page"),
            ({"document": "guide", "section": "12.x", "page": 101}, "invalid spec section '12.x'"),
            ({"document": "guide", "section": "12.1", "page": -1}, "invalid spec page"),
        ):
            with self.subTest(ref=ref):
                reg = registry()
                form(reg, "S_ADD_U32")["spec"].append(ref)
                self.rejects(reg, f"S_ADD_U32: {fragment}")

    def test_rejects_guide_reference_without_a_page(self):
        reg = registry()
        form(reg, "S_ADD_U32")["spec"] = [{"document": "guide", "section": "12.1", "page": 0}]
        self.rejects(reg, "S_ADD_U32: missing a guide section and page reference")

    def test_rejects_opcode_outside_the_encoding_identifiers(self):
        snap = snapshot()
        snap["encodings"]["ENC_SOP2"]["identifiers"].remove(0x80000000 | (14 << 23))
        self.rejects(registry(), "S_AND_B32: opcode 14 is not an identifier of ENC_SOP2", snap)

    def test_rejects_xml_operand_disagreements(self):
        cases = (
            ("missing", lambda ops: ops.pop(2), "S_ADD_U32: XML has no operand SSRC1"),
            ("width", lambda ops: ops[1].__setitem__(3, 64), "S_ADD_U32: SSRC0 is 64 bits in the XML"),
            ("direction", lambda ops: ops[0].__setitem__(4, True),
             "S_ADD_U32: SDST direction disagrees with the XML"),
            ("extra", lambda ops: ops.append(["SIMM16", U32, "OPR_SIMM16", 16, True, False, False, True]),
             "S_ADD_U32: XML operands ['SIMM16'] are not covered by rule sop2_sgpr"),
        )
        for label, change, fragment in cases:
            with self.subTest(case=label):
                snap = snapshot()
                change(snap["variants"][("S_ADD_U32", "ENC_SOP2")]["operands"])
                self.rejects(registry(), fragment, snap)


def _reorder(value):
    """Reverse the key order of every mapping, keeping list order."""
    if isinstance(value, dict):
        return {key: _reorder(value[key]) for key in reversed(list(value))}
    if isinstance(value, list):
        return [_reorder(item) for item in value]
    return value


class Render(unittest.TestCase):
    def test_two_renders_are_identical(self):
        self.assertEqual(gen.render(registry(), snapshot()), gen.render(registry(), snapshot()))

    def test_render_ignores_mapping_key_order(self):
        reg = registry()
        reordered = _reorder(reg)
        self.assertNotEqual(list(reg["form"][0]), list(reordered["form"][0]))
        self.assertEqual(gen.render(reg, snapshot()), gen.render(reordered, snapshot()))

    def test_dispatch_table_is_indexed_by_opcode(self):
        reg = registry()
        reg["form"].reverse()
        text = gen.render(reg, snapshot())
        body = text.split("const SCALAR_ALU_OP_BY_OPCODE: [Option<ScalarAluOp>; 128] = [", 1)[1].split("];", 1)[0]
        rows = [line.strip() for line in body.splitlines() if line.strip()]
        self.assertEqual(len(rows), 128)
        declared = {index: row for index, row in enumerate(rows) if row != "None,"}
        self.assertEqual(sorted(declared), [0, 2, 4, 14, 34])
        self.assertEqual(declared[34], "Some(ScalarAluOp::AshrI32),")
        self.assertIn("if word & 0xc000_0000 != 0x8000_0000 {", text)
        self.assertIn("SCALAR_ALU_OP_BY_OPCODE[((word >> 23) & 0x7f) as usize]", text)

    def test_rejects_identifiers_that_do_not_share_the_encoding_bits(self):
        snap = snapshot()
        snap["encodings"]["ENC_SOP2"]["identifiers"].append(0x40000000)
        with self.assertRaisesRegex(ValueError, "do not split into fixed bits and OP"):
            gen.render(registry(), snap)

    def test_real_registry_renders_the_committed_file(self):
        text = gen.render(gen.load_registry(), gen.load_snapshot())
        self.assertEqual(text, gen.RUST.read_text(encoding="utf-8"))


class LoadRegistry(unittest.TestCase):
    """The root file plus its per-family files merge into one registry."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        (self.dir / "rdna2").mkdir()

    def write(self, root_extra, files):
        (self.dir / "rdna2.toml").write_text(
            "schema = 1\n" + root_extra, encoding="utf-8"
        )
        for name, text in files.items():
            (self.dir / "rdna2" / name).write_text(text, encoding="utf-8")
        return self.dir / "rdna2.toml"

    RULE = '[operand_rules.{name}]\nfamily = "{family}"\n'
    FORM = '[[form]]\nname = "{name}"\nfamily = "{family}"\n'

    def fails(self, root, fragment):
        with self.assertRaises(gen.RegistryError) as raised:
            gen.load_registry(root)
        self.assertIn(fragment, str(raised.exception))

    def test_a_standalone_registry_needs_no_family_directory(self):
        (self.dir / "rdna2").rmdir()
        root = self.write('[[form]]\nname = "S_ADD_U32"\nfamily = "SOP2"\n', {})
        self.assertEqual(gen.load_registry(root)["form"][0]["name"], "S_ADD_U32")
        root.write_text("schema = 1\ninclude = []\n", encoding="utf-8")
        self.assertEqual(gen.load_registry(root)["form"], [])

    def test_a_malformed_variant_list_is_a_registry_error(self):
        self.fails(self.write("encoding_variant = 1\n", {}), "encoding_variant must be an array")
        root = self.write('include = ["rdna2/vop3.toml"]\n', {"vop3.toml": "encoding_variant = 1\n"})
        self.fails(root, "vop3.toml: encoding_variant must be an array")

    def test_the_real_split_registry_equals_one_concatenated_file(self):
        registry = gen.load_registry()
        self.assertNotIn("include", registry)
        families = [form["family"] for form in registry["form"]]
        # Each family is one contiguous run, in include order.
        runs = [family for index, family in enumerate(families) if index == 0 or families[index - 1] != family]
        self.assertEqual(len(runs), len(set(runs)))
        declared = set(runs) | {variant["family"] for variant in registry.get("encoding_variant", [])}
        for name, rule in registry["operand_rules"].items():
            self.assertIn(rule["family"], declared, name)
        # The whole merged result equals the files concatenated by hand: every
        # form, rule and variant, in order, with nothing dropped or reordered.
        import tomllib

        with open(gen.REGISTRY, "rb") as handle:
            expected = tomllib.load(handle)
        includes = expected.pop("include")
        expected.setdefault("operand_rules", {})
        expected.setdefault("form", [])
        for include in includes:
            with open(gen.REGISTRY.parent / include, "rb") as handle:
                part = tomllib.load(handle)
            expected["operand_rules"].update(part.get("operand_rules", {}))
            expected["form"] += part.get("form", [])
            if "encoding_variant" in part:
                expected.setdefault("encoding_variant", []).extend(part["encoding_variant"])
        self.assertEqual(registry, expected)

    def test_merges_family_files_in_include_order(self):
        root = self.write('include = ["rdna2/vop2.toml", "rdna2/sop2.toml"]\n', {
            "vop2.toml": self.RULE.format(name="v", family="VOP2") + self.FORM.format(name="B", family="VOP2"),
            "sop2.toml": self.FORM.format(name="A", family="SOP2") + self.FORM.format(name="C", family="SOP2"),
        })
        registry = gen.load_registry(root)
        self.assertEqual([form["name"] for form in registry["form"]], ["B", "A", "C"])
        self.assertEqual(list(registry["operand_rules"]), ["v"])

    def test_rejects_a_form_of_another_family(self):
        root = self.write('include = ["rdna2/sop2.toml"]\n', {
            "sop2.toml": self.FORM.format(name="V_X", family="VOP2"),
        })
        self.fails(root, "rdna2/sop2.toml: form V_X is not a SOP2 form")

    def test_rejects_a_rule_of_another_family(self):
        root = self.write('include = ["rdna2/sop2.toml"]\n', {
            "sop2.toml": self.RULE.format(name="r", family="SOPC"),
        })
        self.fails(root, "rdna2/sop2.toml: operand rule r is not a SOP2 rule")

    def test_rejects_a_rule_declared_in_two_files(self):
        root = self.write('include = ["rdna2/sop2.toml", "rdna2/sop1.toml"]\n', {
            "sop2.toml": self.RULE.format(name="r", family="SOP2"),
            "sop1.toml": self.RULE.format(name="r", family="SOP1"),
        })
        self.fails(root, "rdna2/sop1.toml: operand rule r is declared twice")

    def test_rejects_root_keys_in_a_family_file(self):
        root = self.write('include = ["rdna2/sop2.toml"]\n', {"sop2.toml": "schema = 1\n"})
        self.fails(root, "rdna2/sop2.toml: keys ['schema'] belong in the root file")

    def test_rejects_includes_outside_the_family_directory(self):
        for include in ("../sop2.toml", "./rdna2/sop2.toml", "other/sop2.toml", "rdna2/SOP2.toml",
                        "rdna2/sub/sop2.toml"):
            with self.subTest(include=include):
                root = self.write(f'include = ["{include}"]\n', {"sop2.toml": ""})
                self.fails(root, f"include {include!r} is not rdna2/<family>.toml")

    def test_rejects_a_family_file_that_is_a_link(self):
        outside = self.dir / "outside.toml"
        outside.write_text("", encoding="utf-8")
        (self.dir / "rdna2" / "sop2.toml").symlink_to(outside)
        root = self.write('include = ["rdna2/sop2.toml"]\n', {})
        self.fails(root, "included family file rdna2/sop2.toml is a link or outside rdna2/")

    def test_rejects_a_linked_directory_or_a_hard_link_alias(self):
        root = self.write('include = ["rdna2/sop2.toml", "rdna2/vop2.toml"]\n', {"sop2.toml": ""})
        os.link(self.dir / "rdna2" / "sop2.toml", self.dir / "rdna2" / "vop2.toml")
        self.fails(root, "included family file rdna2/vop2.toml is another name for an included file")
        outside = self.dir / "outside"
        (self.dir / "rdna2").rename(outside)
        (self.dir / "rdna2").symlink_to(outside, target_is_directory=True)
        self.fails(root, "the family directory rdna2/ is a link")
        (self.dir / "rdna2").unlink()
        (self.dir / "rdna2").symlink_to(self.dir / "rdna2", target_is_directory=True)
        self.fails(root, "the family directory rdna2/ is a link")

    def test_rejects_a_missing_or_repeated_include(self):
        self.fails(self.write('include = ["rdna2/sop2.toml"]\n', {}), "included family file rdna2/sop2.toml does not exist")
        root = self.write('include = ["rdna2/sop2.toml", "rdna2/sop2.toml"]\n', {"sop2.toml": ""})
        self.fails(root, "a family file is included twice")


class CommandLine(unittest.TestCase):
    def run_cli(self, *arguments):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *map(str, arguments)],
            capture_output=True, text=True, check=False,
        )

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)

    def test_check_passes_on_the_committed_file(self):
        result = self.run_cli("--check")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_check_passes_on_a_fresh_file_and_fails_on_a_stale_one(self):
        output = self.dir / "semantics_gen.rs"
        result = self.run_cli("generate", "--output", output)
        self.assertEqual(result.returncode, 0, result.stderr)
        fresh = output.read_text(encoding="utf-8")
        self.assertEqual(self.run_cli("--check", "--output", output).returncode, 0)

        output.write_text(fresh.replace("pub const GENERATED_SCHEMA: u32 = 1;", "pub const GENERATED_SCHEMA: u32 = 2;"))
        stale = self.run_cli("--check", "--output", output)
        self.assertEqual(stale.returncode, 1)
        self.assertIn("is stale", stale.stderr)
        # --check never writes.
        self.assertNotEqual(output.read_text(encoding="utf-8"), fresh)

    def test_check_fails_when_the_generated_file_is_missing(self):
        self.assertEqual(self.run_cli("--check", "--output", self.dir / "absent.rs").returncode, 1)

    def test_invalid_registry_fails_every_mode_without_writing(self):
        bad = self.dir / "rdna2.toml"
        shutil.copytree(gen.REGISTRY.parent / "rdna2", self.dir / "rdna2")
        text = gen.REGISTRY.read_text(encoding="utf-8")
        bad.write_text(text.replace("\nschema = 1\n", "\nschema = 2\n", 1), encoding="utf-8")
        output = self.dir / "out.rs"
        for mode in (("validate",), ("generate",), ("--check",)):
            result = self.run_cli(*mode, "--registry", bad, "--output", output)
            self.assertEqual(result.returncode, 1, mode)
            self.assertIn("error: schema 2 is not 1", result.stderr)
        self.assertFalse(output.exists())

    def test_validate_reports_the_form_count(self):
        result = self.run_cli("validate")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), f"registry valid: {len(gen.load_registry()['form'])} forms")


class SopkForms(unittest.TestCase):
    """SOPK immediate forms, checked against the real registry and XML snapshot."""

    @classmethod
    def setUpClass(cls):
        cls.snapshot = gen.load_snapshot()

    def registry(self):
        return gen.load_registry()

    def rejects(self, reg, fragment):
        errors = gen.validate(reg, self.snapshot)
        self.assertTrue(
            any(fragment in error for error in errors),
            f"expected an error containing {fragment!r}, got {errors}",
        )

    def test_the_registry_declares_the_sixteen_sopk_forms(self):
        names = {item["name"] for item in self.registry()["form"] if item["family"] == "SOPK"}
        self.assertEqual(len(names), 16)
        self.assertTrue({"S_MOVK_I32", "S_CMOVK_I32", "S_ADDK_I32", "S_MULK_I32"} <= names)

    def test_rejects_a_compare_with_the_wrong_sopk_opcode(self):
        reg = self.registry()
        form(reg, "S_CMPK_GT_U32")["opcode"] = 10
        self.rejects(reg, "S_CMPK_GT_U32: relation gt/unsigned has SOPK opcode 11")

    def test_rejects_an_unsigned_compare_declared_signed(self):
        reg = self.registry()
        form(reg, "S_CMPK_GT_U32")["signedness"] = "signed"
        self.rejects(reg, "SIMM16 is 16 bits FMT_NUM_U16")

    def test_rejects_addk_without_the_overflow_flag(self):
        reg = self.registry()
        form(reg, "S_ADDK_I32")["scc"] = "unchanged"
        self.rejects(reg, "S_ADDK_I32: scc unchanged contradicts add/signed")

    def test_rejects_cmovk_that_does_not_read_scc(self):
        reg = self.registry()
        form(reg, "S_CMOVK_I32")["carry_in"] = "none"
        self.rejects(reg, "S_CMOVK_I32: carry_in none contradicts the XML SCC input")

    def test_rejects_a_conditional_move_that_does_not_read_scc(self):
        # S_MOVK_I32 has no SCC input, so as a cmov it would see SCC = 0 and
        # never write D.
        reg = self.registry()
        form(reg, "S_MOVK_I32")["operation"] = "cmov"
        self.rejects(reg, "S_MOVK_I32: operation cmov reads SCC (requires carry_in scc)")

    def test_rejects_a_two_source_operation_on_a_sopk_rule(self):
        reg = self.registry()
        form(reg, "S_MULK_I32")["operation"] = "and"
        form(reg, "S_MULK_I32")["signedness"] = "bitwise"
        self.rejects(reg, "S_MULK_I32: no primitive implements and/bitwise")

    def test_rejects_a_move_on_the_compare_rule(self):
        reg = self.registry()
        form(reg, "S_MOVK_I32")["operands"] = "sopk_compare"
        self.rejects(reg, "S_MOVK_I32: operation mov does not fit rule sopk_compare")

    def test_generated_compare_extends_signed_and_unsigned_immediates_differently(self):
        text = gen.render(self.registry(), self.snapshot)
        self.assertIn("Self::CmpkGtI32 => primitive::gt_i32(dst, primitive::sext_simm16(simm16)),", text)
        self.assertIn("Self::CmpkGtU32 => primitive::gt_u32(dst, simm16 as u32),", text)


if __name__ == "__main__":
    unittest.main()
