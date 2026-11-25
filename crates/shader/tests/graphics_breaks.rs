// SPDX-License-Identifier: GPL-3.0-or-later
use xenora_shader::{Architecture::Gfx1030, compile_graphics_pipeline};
#[test]
fn graphics_countdown_break_before_update_compiles() {
    let compiled = compile_graphics_pipeline(
        Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
        include_bytes!("../../../fixtures/rdna2/x9-loop-break-before-ps.bin"),
    )
    .expect("bounded countdown with a conditional exit must compile");
    assert_eq!(compiled.required_uniform_mask, 3);
}

// Original instruction streams assembled from the encodings in the existing
// original countdown fixtures. Labels resolve signed word-relative branches;
// expected metadata and acceptance are literal, not compiler-derived.
struct Program {
    words: Vec<u32>,
    labels: std::collections::HashMap<&'static str, usize>,
    branches: Vec<(usize, &'static str)>,
}
impl Program {
    fn new() -> Self {
        Self {
            words: Vec::new(),
            labels: Default::default(),
            branches: Vec::new(),
        }
    }
    fn emit(&mut self, words: &[u32]) {
        self.words.extend_from_slice(words);
    }
    fn label(&mut self, name: &'static str) {
        assert!(self.labels.insert(name, self.words.len()).is_none());
    }
    fn branch(&mut self, conditional: bool, target: &'static str) {
        self.branches.push((self.words.len(), target));
        self.words
            .push(if conditional { 0xbf840000 } else { 0xbf820000 });
    }
    fn bytes(mut self) -> Vec<u8> {
        for (at, target) in self.branches {
            let delta = i16::try_from(self.labels[target] as isize - at as isize - 1).unwrap();
            self.words[at] |= u32::from(delta as u16);
        }
        self.words.into_iter().flat_map(u32::to_le_bytes).collect()
    }
}
fn compile(pixel: &[u8]) -> Result<xenora_shader::CompiledGraphics, xenora_shader::CompileError> {
    compile_graphics_pipeline(
        Gfx1030,
        include_bytes!("../../../fixtures/rdna2/x9-constant-vs.bin"),
        pixel,
    )
}
fn prefix(p: &mut Program) {
    // MIN s32,s24,4; MOV s33,3; initialized float accumulator and increment.
    p.emit(&[
        0x83a08418, 0xbea10383, 0x7e100280, 0x7e1202f2, 0x7e1402ff, 0x3e000000,
    ]);
}
fn header(p: &mut Program) {
    p.label("header");
    p.emit(&[0xbf078020]);
    p.branch(true, "exit");
}
fn finish(p: &mut Program) {
    p.label("exit");
    // Consume actual exit SCC before a new compare overwrites it.
    p.branch(true, "scalar_check");
    p.emit(&[0x7e1002f2]);
    p.label("scalar_check");
    p.emit(&[0xbf061921]); // CMP_EQ s33,s25: consumes merged U32.
    p.branch(true, "exports");
    p.emit(&[0x7e1002f2]);
    p.label("exports");
    p.emit(&[0xf800180f, 0x09080808, 0xbf810000]);
}

#[test]
fn breaks_before_after_decrement_and_multiple_sites_merge_typed_live_outs() {
    for after in [false, true] {
        let mut p = Program::new();
        prefix(&mut p);
        header(&mut p);
        if after {
            p.emit(&[0x80a08120]);
        }
        p.emit(&[0xbf071920]); // counter != s25
        p.branch(true, "exit");
        p.emit(&[0x80218521, 0x0610110a]); // scalar +=5; float +=.125
        // A true-SCC exit via a diamond must differ from header exhaustion SCC.
        p.emit(&[0xbf061921]);
        p.branch(true, "continue");
        p.branch(false, "exit");
        p.label("continue");
        if !after {
            p.emit(&[0x80a08120]);
        }
        p.branch(false, "header");
        finish(&mut p);
        let compiled = compile(&p.bytes()).unwrap();
        assert_eq!(compiled.required_uniform_mask, 3);
        assert_eq!(compiled.sampled_pairs, 0);
        assert!(compiled.shader.msl.contains("while (!finished)"));
    }
}

#[test]
fn preheader_diamond_merge_precedes_loop_header_initialization() {
    let mut p = Program::new();
    prefix(&mut p);
    p.emit(&[0xbf061918]); // CMP_EQ s24,s25
    p.branch(true, "other");
    p.emit(&[0xbea10387]); // s33=7
    p.branch(false, "header");
    p.label("other");
    p.emit(&[0xbea1038b]); // s33=11
    header(&mut p);
    p.emit(&[0x80218521, 0xbf071920]);
    p.branch(true, "exit");
    p.emit(&[0x80a08120]);
    p.branch(false, "header");
    finish(&mut p);
    let compiled = compile(&p.bytes()).unwrap();
    assert_eq!(compiled.required_uniform_mask, 3);
    // Native compilation is the separate oracle for scoped temporary declarations.
    assert!(compiled.shader.msl.contains("edge_tmp0"));
}

#[test]
fn break_does_not_allow_a_continuing_path_to_skip_decrement() {
    let mut p = Program::new();
    prefix(&mut p);
    header(&mut p);
    p.emit(&[0xbf071920]);
    p.branch(true, "latch");
    p.emit(&[0x80a08120]);
    p.label("latch");
    p.branch(false, "header");
    finish(&mut p);
    assert!(
        compile(&p.bytes())
            .unwrap_err()
            .to_string()
            .contains("decrement does not dominate latch")
    );
}

#[test]
fn break_exit_cannot_supply_definition_missing_on_zero_trip() {
    let mut p = Program::new();
    p.emit(&[0x83a08418, 0x7e100280, 0x7e1202f2]);
    header(&mut p);
    p.emit(&[0xbea10383, 0xbf071920]);
    p.branch(true, "exit");
    p.emit(&[0x80a08120]);
    p.branch(false, "header");
    finish(&mut p);
    assert!(
        compile(&p.bytes())
            .unwrap_err()
            .to_string()
            .contains("uninitialized")
    );
}

#[test]
fn nested_break_continues_outer_loop_but_cannot_exit_ancestor() {
    for cross_level in [false, true] {
        let mut p = Program::new();
        prefix(&mut p);
        header(&mut p);
        p.emit(&[0x83a18419]); // inner s33=MIN(s25,4), fresh each outer iteration
        p.label("inner");
        p.emit(&[0xbf078021]);
        p.branch(true, "inner_exit");
        p.emit(&[0xbf071a21]); // inner counter != s26
        p.branch(true, if cross_level { "exit" } else { "inner_exit" });
        p.emit(&[0x0610110a, 0x80a18121]);
        p.branch(false, "inner");
        p.label("inner_exit");
        // Keep the inner exit SCC live before the outer decrement.
        p.branch(true, "outer_update");
        p.emit(&[0x7e1002f2]);
        p.label("outer_update");
        p.emit(&[0x80a08120]);
        p.branch(false, "header");
        finish(&mut p);
        let result = compile(&p.bytes());
        if cross_level {
            assert!(result.unwrap_err().to_string().contains("unsupported exit"));
        } else {
            assert_eq!(result.unwrap().required_uniform_mask, 7);
        }
    }
}

#[test]
fn proven_zero_break_loop_prunes_body_uniforms() {
    let mut bytes = include_bytes!("../../../fixtures/rdna2/x9-loop-break-before-ps.bin").to_vec();
    // Original first instruction MIN s32,s24,4 becomes MIN s32,0,4.
    bytes[..4].copy_from_slice(&0x83a08480u32.to_le_bytes());
    let compiled = compile(&bytes).unwrap();
    assert_eq!(compiled.required_uniform_mask, 0);
    assert_eq!(compiled.sampled_pairs, 0);
    assert_eq!(compiled.color_targets, 1);
}

#[test]
fn arbitrary_exit_is_rejected_even_from_a_proven_zero_body() {
    let mut bytes = include_bytes!("../../../fixtures/rdna2/x9-loop-break-before-ps.bin").to_vec();
    bytes[..4].copy_from_slice(&0x83a08480u32.to_le_bytes());
    // Branch at32 now targets END at56, bypassing the common export continuation48.
    bytes[32..36].copy_from_slice(&0xbf840005u32.to_le_bytes());
    assert!(
        compile(&bytes)
            .unwrap_err()
            .to_string()
            .contains("unsupported exit")
    );
}

#[test]
fn reused_break_counter_requires_a_fresh_explicit_bound() {
    for rebound in [false, true] {
        let mut p = Program::new();
        prefix(&mut p);
        header(&mut p);
        p.emit(&[0x80a08120, 0xbf071920]);
        p.branch(true, "exit");
        p.branch(false, "header");
        p.label("exit");
        if rebound {
            p.emit(&[0x83a08420]); // MIN s32,s32,4, explicit bound on exit phi
        }
        p.label("second_header");
        p.emit(&[0xbf078020]);
        p.branch(true, "done");
        p.emit(&[0x0610110a, 0x80a08120]);
        p.branch(false, "second_header");
        p.label("done");
        p.emit(&[0xf800180f, 0x09080808, 0xbf810000]);
        let result = compile(&p.bytes());
        if rebound {
            assert_eq!(result.unwrap().required_uniform_mask, 3);
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("trip count lacks a proven bound")
            );
        }
    }
}

#[test]
fn constant_break_prunes_unbounded_child_but_runtime_break_does_not() {
    for runtime in [false, true] {
        let mut p = Program::new();
        prefix(&mut p);
        header(&mut p);
        if runtime {
            p.emit(&[0xbf071920]);
        }
        p.branch(runtime, "exit");
        p.emit(&[0xbea10319]); // s33=s25: deliberately unbounded child count
        p.label("inner");
        p.emit(&[0xbf078021]);
        p.branch(true, "inner_exit");
        p.emit(&[0x0610110a, 0x80a18121]);
        p.branch(false, "inner");
        p.label("inner_exit");
        p.emit(&[0x80a08120]);
        p.branch(false, "header");
        p.label("exit");
        p.emit(&[0xf800180f, 0x09080808, 0xbf810000]);
        let result = compile(&p.bytes());
        if runtime {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("trip count lacks a proven bound")
            );
        } else {
            let compiled = result.unwrap();
            assert_eq!(compiled.required_uniform_mask, 1);
            assert_eq!(compiled.sampled_pairs, 0);
        }
    }
}
