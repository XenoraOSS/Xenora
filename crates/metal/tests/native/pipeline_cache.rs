// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from katherinebytes, upstream commit 6f1ac2a906ff817f4d933c799cd1f8fa4822cbf9.
// Synchronous calls replace the upstream submission-token cases.
use super::{context, fixture, native};
use xenora_metal::NativeCompute;

fn pipeline_was_reused(profile: &xenora_metal::RunProfile) -> bool {
    profile.library_seconds == 0.0 && profile.pipeline_seconds == 0.0
}

#[test]
fn compute_pipeline_cache_distinguishes_source_entry_and_context() {
    let context = context();
    let source = fixture("output[id] = input[id] + 1u;");
    let second_entry =
        fixture("output[id] = input[id] * 2u;").replace("void fixture(", "void other(");
    let source = format!("{source}\n{second_entry}");
    let changed = source.replace("input[id] + 1u", "input[id] - 1u");
    let input = [0, 11, u32::MAX];

    for (msl, entry, expected) in [
        (source.as_str(), "fixture", [1, 12, 0]),
        (source.as_str(), "other", [0, 22, u32::MAX - 1]),
        (changed.as_str(), "fixture", [u32::MAX, 10, u32::MAX - 1]),
    ] {
        let cold = native(context.run_words_profiled(msl, entry, &input));
        let warm = native(context.run_words_profiled(msl, entry, &input));
        assert!(!pipeline_was_reused(&cold), "{entry}");
        assert!(pipeline_was_reused(&warm), "{entry}");
        assert_eq!(cold.output, expected, "{entry}");
        assert_eq!(warm.output, expected, "{entry}");
    }
    let separate = native(NativeCompute::new());
    let fresh = native(separate.run_words_profiled(&source, "fixture", &input));
    assert!(!pipeline_was_reused(&fresh));
    assert_eq!(fresh.output, [1, 12, 0]);

    for _ in 0..2 {
        for (msl, entry, operation) in [
            (source.as_str(), "missing", "newFunctionWithName"),
            ("not valid MSL", "fixture", "newLibraryWithSource"),
        ] {
            let error = context
                .run_words_profiled(msl, entry, &input)
                .expect_err("invalid pipeline must still fail after warming the cache");
            assert_eq!(error.operation, operation);
        }
    }
    let error = context
        .run_words_with_group_width(&source, "fixture", &input, 0)
        .expect_err("cached pipeline must still check dispatch width");
    assert_eq!(error.operation, "threadgroup width");
}

#[test]
fn compute_pipeline_cache_bounds_key_bytes_without_rejecting_maximum_source() {
    const SOURCE_LIMIT: usize = 64 * 1024;
    let context = context();
    let mut source = fixture("output[id] = input[id] ^ 1u;");
    source.push_str("\n// ");
    source.push_str(&" ".repeat(SOURCE_LIMIT - source.len()));

    for _ in 0..2 {
        let profile = native(context.run_words_profiled(&source, "fixture", &[8]));
        assert_eq!(profile.output, [9]);
        assert!(!pipeline_was_reused(&profile));
    }
    source.push(' ');
    let error = context
        .run_words_profiled(&source, "fixture", &[8])
        .expect_err("source above 64 KiB must fail before pipeline lookup");
    assert_eq!(error.operation, "source bounds");
}

#[test]
fn compute_pipeline_cache_evicts_least_recent_key() {
    let context = context();
    // Seventeen entries in one source fit in Metal's input limit and exceed
    // Xenora's 16-pipeline cache. Each kernel writes a distinct result.
    let source: String = (0..=16)
        .map(|id| {
            fixture(&format!("output[id] = input[id] + {id}u;"))
                .replace("void fixture(", &format!("void entry{id}("))
        })
        .collect::<Vec<_>>()
        .join("\n");
    native(context.max_group_width(&source, "entry0"));
    native(context.max_group_width(&source, "entry1"));
    for id in 2..16 {
        native(context.max_group_width(&source, &format!("entry{id}")));
    }
    native(context.max_group_width(&source, "entry0"));
    native(context.max_group_width(&source, "entry16"));

    let recent = native(context.run_words_profiled(&source, "entry0", &[29]));
    assert!(pipeline_was_reused(&recent));
    assert_eq!(recent.output, [29]);
    let rebuilt = native(context.run_words_profiled(&source, "entry1", &[17]));
    assert!(!pipeline_was_reused(&rebuilt));
    assert_eq!(rebuilt.output, [18]);
}

#[test]
fn compute_pipeline_cache_concurrent_calls_share_one_build() {
    let context = context();
    let source = fixture("output[id] = input[id] ^ 5u;");
    let start = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|input| {
                let (context, source, start) = (&context, &source, &start);
                scope.spawn(move || {
                    start.wait();
                    let result = native(context.run_words_profiled(source, "fixture", &[input]));
                    assert_eq!(result.output, [input ^ 5]);
                    usize::from(!pipeline_was_reused(&result))
                })
            })
            .collect();
        let builds: usize = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .sum();
        assert_eq!(builds, 1);
    });
}
