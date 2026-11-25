"""Validation and freshness tests for crates/loader/generate_x86.py."""

import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("generate_x86", ROOT / "crates/loader/generate_x86.py")
generate_x86 = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(generate_x86)

REGISTRY = (ROOT / "crates/loader/x86/instructions.toml").read_text()


class GenerateX86(unittest.TestCase):
    def rejects(self, text, message):
        with self.assertRaisesRegex(generate_x86.RegistryError, message):
            generate_x86.load(text)

    def form(self, body):
        return REGISTRY + "\n[[form]]\n" + body

    def test_committed_table_is_fresh_and_deterministic(self):
        first = generate_x86.generate(*generate_x86.load(REGISTRY))
        self.assertEqual(first, generate_x86.generate(*generate_x86.load(REGISTRY)))
        self.assertEqual(first, (ROOT / "crates/loader/src/x86_table.rs").read_text())

    def test_sign_extension_cannot_claim_mov_immediate_opcodes(self):
        # The defect that started this work: BE/BF in the one-byte map.
        for opcode in (0xBE, 0xBF):
            self.rejects(
                self.form(f'op = "movsx"\nopcode = {opcode}\nenc = "reg_rm"\nsize = "v"\nsrc = "b"'),
                "overlapping encoding")

    def test_every_mov_immediate_opcode_is_a_mov(self):
        _, forms = generate_x86.load(REGISTRY)
        for opcode in range(0xB0, 0xC0):
            owners = [f["op"] for f in forms if f["map"] == "1"
                      and f["opcode"] <= opcode < f["opcode"] + f["count"]]
            self.assertEqual(owners, ["mov"])

    def test_overlaps_are_rejected(self):
        self.rejects(self.form('op = "nop"\nopcode = 0x90\nenc = "none"\nsize = "n"'), "overlapping")
        self.rejects(self.form('op = "nop"\nopcode = 0x57\nenc = "none"\nsize = "n"'), "overlapping")
        self.rejects(self.form('op = "not"\nopcode = 0xF7\ndigit = 2\nenc = "rm"\nsize = "v"'), "overlapping")
        self.rejects(self.form('op = "not"\nopcode = 0xF7\nenc = "rm"\nsize = "v"'), "overlapping")

    def test_prefixes_and_escapes_are_not_instructions(self):
        for opcode in (0x0F, 0x48, 0x66, 0xF0):
            self.rejects(self.form(f'op = "nop"\nopcode = {opcode}\nenc = "none"\nsize = "n"'), "prefix or escape")

    def test_contradictory_operands_are_rejected(self):
        self.rejects(self.form('op = "lea"\nopcode = 0xD7\nenc = "reg_rm"\nsize = "v"\nmemory_only = true\nregister_only = true'), "contradict")
        self.rejects(self.form('op = "nop"\nopcode = 0xD7\nenc = "none"\nsize = "n"\nmemory_only = true'), "without ModRM")
        self.rejects(self.form('op = "nop"\nopcode = 0xD7\nenc = "none"\nsize = "n"\nimm = "ib"'), "not valid for none")
        self.rejects(self.form('op = "nop"\nopcode = 0xD7\ndigit = 1\nenc = "none"\nsize = "n"'), "digit needs")
        self.rejects(self.form('op = "nop"\nopcode = 0xD4\ncount = 8\nenc = "none"\nsize = "n"'), "aligned")

    def test_mandatory_prefix_cannot_shadow_an_operand_size(self):
        # 66 0F AF is a 16-bit IMUL, and 66 0F 6E is a MOVD; neither may be both.
        self.rejects(self.form('op = "paddb"\nmap = "0f"\nprefix = "66"\nopcode = 0xAF\nenc = "xmm_rm"\nsize = "x"\nsrc = "x"'), "both an operand size")
        self.rejects(self.form('op = "imul2"\nmap = "0f"\nopcode = 0x6E\nenc = "reg_rm"\nsize = "v"'), "both an operand size")

    def test_vector_operand_rules(self):
        self.rejects(self.form('op = "paddb"\nmap = "0f"\nprefix = "66"\nopcode = 0xF0\nenc = "xmm_rm"\nsize = "x"'), "source size")
        self.rejects(self.form('op = "paddb"\nmap = "0f"\nprefix = "66"\nopcode = 0xF0\nenc = "xmm_rm"\nsize = "v"\nsrc = "x"'), "XMM size")
        self.rejects(self.form('op = "movq"\nmap = "0f"\nprefix = "66"\nopcode = 0xF0\nenc = "xmm_rm"\nsize = "x"\nsrc = "q"\nunaligned = true'), "alignment applies")
        self.rejects(self.form('op = "nop"\nprefix = "66"\nopcode = 0xD7\nenc = "none"\nsize = "n"'), "no opcode table for prefix '66' in map 1")

    def test_three_byte_maps_need_the_66_prefix(self):
        form = 'op = "pshufb"\nmap = "{}"\n{}opcode = 0xF0\nenc = "xmm_rm"\nsize = "x"\nsrc = "x"'
        for opcode_map in ("0f38", "0f3a"):
            for prefix in ("", 'prefix = "f2"\n', 'prefix = "f3"\n'):
                self.rejects(self.form(form.format(opcode_map, prefix)), f"in map {opcode_map}")
            generate_x86.load(self.form(form.format(opcode_map, 'prefix = "66"\n')))
        self.rejects(self.form(form.format("0f39", "")), "unknown map")
        # A form lands in its own map's table and no other.
        operations, forms = generate_x86.load(self.form(form.format("0f3a", 'prefix = "66"\n')))
        index = next(i for i, f in enumerate(forms) if f["map"] == "0f3a" and f["opcode"] == 0xF0)
        tables = generate_x86.generate(operations, forms).split("pub static ")
        holding = [t.split(":")[0] for t in tables if f"Entry::Form({index})" in t]
        self.assertEqual(holding, ["THREE_BYTE_3A_66"])

    def test_a_lane_is_fixed_by_source_or_wide_by_size(self):
        form = 'op = "pinsrb"\nmap = "0f3a"\nprefix = "66"\nopcode = 0xF0\nenc = "{}"\nimm = "ub"\n{}'
        for encoding in ("xmm_gpr_imm", "gpr_xmm_imm"):
            for shape in ('size = "x"', 'size = "x"\nsrc = "q"', 'size = "x"\nsrc = "x"', 'size = "y"\nsrc = "d"', 'size = "y"\nsrc = "b"',
                          'size = "v"', 'size = "q"'):
                self.rejects(self.form(form.format(encoding, shape)), "a lane is")
            for shape in ('size = "x"\nsrc = "b"', 'size = "x"\nsrc = "w"', 'size = "x"\nsrc = "d"', 'size = "y"'):
                generate_x86.load(self.form(form.format(encoding, shape)))

    def test_ptest_is_the_only_packed_integer_operation_that_writes_flags(self):
        operations, forms = generate_x86.load(REGISTRY)
        writers = {form["op"] for form in forms
                   if form["map"] in ("0f38", "0f3a") and operations[form["op"]]["writes"]}
        self.assertEqual(writers, {"ptest"})
        self.assertEqual(operations["ptest"]["writes"], set(generate_x86.FLAGS))

    def test_lock_needs_a_memory_destination(self):
        # Not a form without ModRM, not one whose ModRM operand is the source,
        # and not one that takes only a register.
        message = "LOCK needs a memory destination"
        self.rejects(self.form('op = "nop"\nopcode = 0xD7\nenc = "none"\nsize = "n"\nlock = true'), message)
        self.rejects(self.form('op = "mov"\nopcode = 0xD7\nenc = "reg_rm"\nsize = "v"\nlock = true'), message)
        self.rejects(self.form('op = "xadd"\nopcode = 0xD7\nenc = "rm_reg"\nsize = "v"\nregister_only = true\nlock = true'), message)

    def test_a_fixed_rm_field_needs_a_register_only_digit_form(self):
        base = 'op = "nop"\nopcode = 0xD9\nenc = "rm_unread"\nsize = "n"\n'
        message = "needs register_only and a digit"
        self.rejects(self.form(base + "digit = 0\nrm = 3"), message)
        self.rejects(self.form(base + "digit = 0\nmemory_only = true\nrm = 3"), message)
        self.rejects(self.form(base + "register_only = true\nrm = 3"), message)
        for value in ("8", "-1", "true", '"3"'):
            self.rejects(self.form(base + f"digit = 0\nregister_only = true\nrm = {value}"), "r/m field value 0-7")
        # A second form on the same opcode and digit is an overlap, whatever its r/m field.
        self.rejects(self.form('op = "nop"\nopcode = 0xDB\ndigit = 4\nenc = "rm_unread"\nsize = "n"\n'
                               "register_only = true\nrm = 2"), "overlapping")
        # The valid shape loads, and the key reaches the generated table.
        operations, forms = generate_x86.load(self.form(base + "digit = 0\nregister_only = true\nrm = 7"))
        self.assertEqual([f["rm"] for f in forms if f["map"] == "1" and f["opcode"] == 0xD9], [7])
        self.assertIn("register_only: true, fixed_rm: Some(7),", generate_x86.generate(operations, forms))

    def test_fninit_is_the_only_form_with_a_fixed_rm_field_and_the_only_x87_form(self):
        _, forms = generate_x86.load(REGISTRY)
        fixed = [(f["op"], f["map"], f["opcode"], f["digit"], f["rm"]) for f in forms if f["rm"] is not None]
        self.assertEqual(fixed, [("fninit", "1", 0xDB, 4, 3)])
        x87 = [f["op"] for f in forms if f["map"] == "1" and 0xD8 <= f["opcode"] <= 0xDF]
        self.assertEqual(x87, ["fninit"])

    def test_the_lockable_forms_are_ones_the_sdm_allows_to_lock(self):
        # Intel SDM Vol. 2, LOCK: the prefix is allowed on these instructions,
        # and only when the destination is memory. CMP is not among them.
        sdm = {"add", "adc", "and", "btc", "btr", "bts", "cmpxchg", "cmpxchg8b", "cmpxchg16b",
               "dec", "inc", "neg", "not", "or", "sbb", "sub", "xor", "xadd", "xchg"}
        _, forms = generate_x86.load(REGISTRY)
        lockable = [form for form in forms if form.get("lock")]
        operations = {form["op"] for form in lockable}
        self.assertLessEqual(operations, sdm)
        # The ones the registry marks today (#198, #447, #451): all but
        # CMPXCHG8B and CMPXCHG16B.
        self.assertEqual(operations, sdm - {"cmpxchg8b", "cmpxchg16b"})
        # A form whose destination is a register, or the accumulator, cannot
        # lock, and neither can a form that only takes a register.
        self.assertEqual({form["enc"] for form in lockable}, {"rm_reg", "rm_imm", "rm"})
        self.assertFalse(any(form.get("register_only") for form in lockable))
        self.assertFalse(any(form.get("lock") for form in forms if form["op"] == "cmp"))
        for operation in ("add", "or", "adc", "sbb", "and", "sub", "xor"):
            encodings = {form["enc"]: bool(form.get("lock")) for form in forms if form["op"] == operation}
            self.assertEqual(encodings, {"rm_reg": True, "rm_imm": True, "reg_rm": False, "acc_imm": False},
                             operation)

    def test_a_group_locks_only_its_own_operations(self):
        self.rejects(REGISTRY.replace('lock = ["inc", "dec"]', 'lock = ["inc", "mul"]'), "not in the group")
        self.rejects(REGISTRY.replace('lock = ["bts", "btr", "btc"]', 'lock = ["bts", "shl"]'), "not in the group")

    def test_string_forms_are_movs_and_stos_without_operands(self):
        _, forms = generate_x86.load(REGISTRY)
        self.assertEqual({form["op"] for form in forms if form["enc"] == "string"}, {"movs", "stos"})
        self.rejects(self.form('op = "movs"\nopcode = 0xD7\nenc = "string"\nsize = "b"\nimm = "ib"'), "not valid for string")
        self.rejects(self.form('op = "movs"\nopcode = 0xD7\nenc = "string"\nsize = "b"\nlock = true'), "LOCK needs a memory destination")

    def test_unknown_vocabulary_is_rejected(self):
        self.rejects(self.form('op = "frob"\nopcode = 0xD7\nenc = "none"\nsize = "n"'), "unknown op")
        self.rejects(self.form('op = "nop"\nopcode = 0xD7\nenc = "none"\nsize = "z"'), "unknown size")
        self.rejects(self.form('op = "nop"\nopcode = 0xD7\nenc = "none"\nsize = "n"\nextra = 1'), "unknown keys")

    def test_contradictory_effects_are_rejected(self):
        self.rejects(REGISTRY.replace('bt = { writes = "cf",', 'bt = { writes = "cf sf",', 1), "both written and undefined")
        self.rejects(REGISTRY.replace('clc = { writes = "cf"', 'clc = { writes = "cx"', 1), "unknown flag")
        self.rejects(REGISTRY.replace('ref = "sdm CLC"', 'ref = "blog CLC"', 1), "listed document")


if __name__ == "__main__":
    unittest.main()
