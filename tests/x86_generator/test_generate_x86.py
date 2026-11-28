"""Validation and freshness tests for crates/loader/generate_x86.py."""

import importlib.util
import re
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

    def test_three_byte_maps_need_a_prefix_with_a_table(self):
        form = 'op = "pshufb"\nmap = "{}"\n{}opcode = 0xF0\nenc = "xmm_rm"\nsize = "x"\nsrc = "x"'
        for opcode_map in ("0f38", "0f3a"):
            for prefix in ("", 'prefix = "f3"\n', *(['prefix = "f2"\n'] if opcode_map == "0f3a" else [])):
                self.rejects(self.form(form.format(opcode_map, prefix)), f"in map {opcode_map}")
            generate_x86.load(self.form(form.format(opcode_map, 'prefix = "66"\n')))
        generate_x86.load(self.form('op = "crc32"\nmap = "0f38"\nprefix = "f2"\nopcode = 0xF5\nenc = "reg_rm"\nsize = "v"'))
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

    def test_only_ptest_and_the_string_compares_write_flags_in_the_three_byte_maps(self):
        operations, forms = generate_x86.load(REGISTRY)
        writers = {form["op"] for form in forms
                   if form["map"] in ("0f38", "0f3a") and operations[form["op"]]["writes"]}
        self.assertEqual(writers, {"ptest", "pcmpestri", "pcmpestrm", "pcmpistri", "pcmpistrm"})
        for op in writers:
            self.assertEqual(operations[op]["writes"], set(generate_x86.FLAGS))

    def test_length_registers_set_the_operand_size_of_a_string_compare(self):
        form = 'op = "pcmpestri"\nmap = "0f3a"\nprefix = "66"\nopcode = 0xF0\nenc = "xmm_rm_length"\nimm = "ub"\n{}'
        generate_x86.load(self.form(form.format('size = "y"\nsrc = "x"')))
        self.rejects(self.form(form.format('size = "x"\nsrc = "x"')), "XMM size")
        self.rejects(self.form(form.format('size = "y"')), "source size")

    def vex(self, **changes):
        entry = {"op": '"vperm2f128"', "map": '"0f3a"', "prefix": '"66"', "opcode": "0xF0", "enc": '"xmm_rm_imm"',
                 "length": '"256"', "w": '"0"', "src": '"v"', "nds": "true"} | changes
        return REGISTRY + "\n[[vex]]\n" + "".join(f"{k} = {v}\n" for k, v in entry.items() if v is not None)

    def test_vex_forms_are_validated(self):
        operations, forms = generate_x86.load(self.vex())
        self.assertEqual(forms[-1]["vex"], {"key": 3 << 10 | 1 << 8 | 0xF0, "digit": None, "length": True, "w": False,
                                            "memory": 0, "nds": True})
        rejected = [({"map": '"1"'}, "unknown map"), ({"prefix": '"67"'}, "unknown prefix"),
                    ({"length": '"512"'}, "unknown length"), ({"length": None}, "unknown length"),
                    ({"w": '"2"'}, "unknown w"), ({"op": '"nothing"'}, "unknown op"), ({"lock": "true"}, "unknown keys"),
                    ({"opcode": "0x100"}, "out of range"), ({"enc": '"gpr_xmm"', "src": None}, "no register"),
                    ({"src": None}, "source size"), ({"src": '"q"', "aligned": "true"}, "alignment"),
                    ({"memory_only": "true", "register_only": "true"}, "contradict"),
                    ({"enc": '"rm_xmm"'}, "a store takes no source"),
                    ({"enc": '"none"', "src": None, "nds": None, "digit": "1"}, "without ModRM"),
                    ({"digit": "8"}, "a digit is 0-7"),
                    ({"enc": '"gpr_xmm_imm"', "src": '"b"'}, "no register"),
                    ({"enc": '"reg_xmm_imm"'}, "no register"), ({"enc": '"xmm_rm_length"'}, "no register"),
                    ({"enc": '"xmm_gpr_imm"'}, "a lane is"), ({"enc": '"xmm_gpr_imm"', "src": '"x"'}, "a lane is"),
                    ({"enc": '"gpr_xmm_imm"', "src": '"q"', "nds": None}, "a lane is"),
                    ({"enc": '"rm_imm_xmm"'}, "needs VEX.vvvv and a register"),
                    ({"enc": '"rm_imm_xmm"', "register_only": "true", "nds": None}, "needs VEX.vvvv and a register")]
        for changes, message in rejected:
            self.rejects(self.vex(**changes), message)

    def test_vex_immediates_follow_the_encoding(self):
        for enc, changes in (("rm_imm_xmm", {"register_only": "true"}), ("xmm_rm_length", {"nds": None}),
                             ("xmm_gpr_imm", {"src": '"w"'}), ("gpr_xmm_imm", {"src": None, "nds": None}),
                             ("reg_xmm_imm", {"nds": None}), ("xmm_rm_imm", {})):
            form = generate_x86.load(self.vex(enc=f'"{enc}"', **changes))[1][-1]
            self.assertEqual(form["imm"], "ub", enc)
        for enc, changes in (("xmm_rm", {}), ("xmm_gpr", {"src": None}), ("reg_xmm", {"nds": None})):
            form = generate_x86.load(self.vex(enc=f'"{enc}"', **changes))[1][-1]
            self.assertEqual(form["imm"], "", enc)

    def test_vex_forms_overlap_only_if_every_field_does(self):
        twice = lambda **second: self.vex() + self.vex(**second)[len(REGISTRY):]
        self.rejects(twice(), "overlapping encoding")
        self.rejects(twice(length='"any"'), "overlapping encoding")
        self.rejects(twice(w='"any"'), "overlapping encoding")
        self.rejects(twice(memory_only="true"), "overlapping encoding")
        for apart in ({"length": '"128"'}, {"w": '"1"'}, {"opcode": "0xF1"}, {"prefix": None}, {"map": '"0f38"'}):
            generate_x86.load(twice(**apart))
        split = self.vex(memory_only="true") + self.vex(register_only="true")[len(REGISTRY):]
        generate_x86.load(split)
        digits = self.vex(digit="1") + self.vex(digit="2")[len(REGISTRY):]
        generate_x86.load(digits)

    def test_vex_forms_never_enter_the_legacy_tables(self):
        operations, forms = generate_x86.load(REGISTRY)
        rust = generate_x86.generate(operations, forms)
        indices = {i for i, form in enumerate(forms) if "vex" in form}
        tables = rust[rust.index("pub static ONE_BYTE"):]
        for index in indices:
            self.assertNotIn(f"Entry::Form({index})", tables)
        keys = re.findall(r"VexForm \{ key: 0x([0-9a-f]+), form: (\d+)", rust)
        self.assertEqual({int(form) for _, form in keys}, indices)
        self.assertEqual([key for key, _ in keys], sorted(key for key, _ in keys))

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
        # The registry marks all of them (#198, #447, #451, #971). CMPXCHG16B
        # is the REX.W form of the cmpxchg8b operation, whose size is 32 or
        # 64 bits by REX.W, so it has no name of its own.
        self.assertEqual(operations, sdm - {"cmpxchg16b"})
        pair = [form for form in forms if form["op"] == "cmpxchg8b"]
        self.assertEqual([(f["map"], f["opcode"], f["digit"], f["size"], f.get("memory_only")) for f in pair],
                         [("0f", 0xC7, 1, "y", True)])
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

    def test_string_forms_are_the_five_string_operations_without_operands(self):
        _, forms = generate_x86.load(REGISTRY)
        self.assertEqual({form["op"] for form in forms if form["enc"] == "string"},
                         {"movs", "stos", "lods", "scas", "cmps"})
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
