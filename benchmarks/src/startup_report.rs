// SPDX-License-Identifier: GPL-3.0-or-later

//! JSON and markdown output of `xenora-bench startup` (#178).

use std::fmt::Write as _;

use crate::startup::{PHASES, Sample, Summary, summarize};
use crate::{Metadata, json_str};

pub(crate) struct Report<'a> {
    pub(crate) fixture: &'a str,
    pub(crate) words: usize,
    pub(crate) fresh: &'a [Sample],
    pub(crate) wall: &'a [f64],
    pub(crate) first: &'a Sample,
    pub(crate) repeated: &'a [Sample],
}

fn column(samples: &[Sample], phase: usize) -> Vec<f64> {
    samples.iter().map(|s| s.0[phase]).collect()
}

fn summary_json(summary: Option<Summary>) -> String {
    summary.map_or_else(
        || "null".to_string(),
        |s| {
            format!(
                "{{\"n\": {}, \"min\": {:e}, \"median\": {:e}, \"p90\": {:e}, \"p99\": {:e}, \"max\": {:e}}}",
                s.n, s.min, s.median, s.p90, s.p99, s.max
            )
        },
    )
}

impl Report<'_> {
    pub(crate) fn json(&self) -> String {
        let mut out = format!(
            "{{\n  \"fixture\": {},\n  \"words\": {},\n  \"process_wall\": {},\n",
            json_str(self.fixture),
            self.words,
            summary_json(summarize(self.wall))
        );
        for (regime, samples) in [("fresh", self.fresh), ("repeated", self.repeated)] {
            let _ = writeln!(out, "  \"{regime}\": {{");
            for (i, phase) in PHASES.iter().enumerate() {
                let comma = if i + 1 < PHASES.len() { "," } else { "" };
                let summary = if regime == "repeated" && i < 2 {
                    None
                } else {
                    summarize(&column(samples, i))
                };
                let _ = writeln!(out, "    \"{phase}\": {}{comma}", summary_json(summary));
            }
            out.push_str("  },\n");
        }
        let first: Vec<String> = PHASES
            .iter()
            .zip(self.first.0)
            .skip(1)
            .map(|(p, v)| format!("\"{p}\": {v:e}"))
            .collect();
        let _ = writeln!(out, "  \"first_in_process\": {{{}}},", first.join(", "));
        // Raw fresh-process samples in launch order, so a cold first process
        // (for example an empty Metal shader cache) stays visible.
        let rows: Vec<String> = self
            .fresh
            .iter()
            .zip(self.wall)
            .map(|(sample, wall)| {
                let phases: Vec<String> = sample.0.iter().map(|v| format!("{v:e}")).collect();
                format!("[{}, {wall:e}]", phases.join(", "))
            })
            .collect();
        let _ = writeln!(
            out,
            "  \"fresh_samples_columns\": [{}, \"process_wall\"],\n  \"fresh_samples\": [\n    {}\n  ]\n}}",
            PHASES
                .iter()
                .map(|p| json_str(p))
                .collect::<Vec<_>>()
                .join(", "),
            rows.join(",\n    ")
        );
        out
    }

    pub(crate) fn markdown(&self, metadata: &Metadata) -> String {
        let ms = |v: f64| format!("{:.3}", v * 1e3);
        let mut out = format!(
            "# Metal startup: `{}`, {} words\n\nDevice {}, {} build, commit `{}`, API validation `{}`, shader validation `{}`, power `{}`, thermal `{}`.\n\nAll values in milliseconds. Fresh: {} processes. Repeated: {} runs after the first in one process. Every sample produced the expected output.\n\n| Phase | Fresh median | Fresh p90 | Fresh p99 | Fresh max | First in process | Repeated median | Repeated p90 | Repeated p99 | Repeated max |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
            self.fixture,
            self.words,
            metadata.device,
            metadata.profile,
            metadata.commit,
            metadata.api_validation,
            metadata.shader_validation,
            metadata.power,
            metadata.thermal,
            self.fresh.len(),
            self.repeated.len(),
        );
        for (i, phase) in PHASES.iter().enumerate() {
            let fresh = summarize(&column(self.fresh, i));
            let repeated = (i >= 2)
                .then(|| summarize(&column(self.repeated, i)))
                .flatten();
            let cells = |s: Option<Summary>| {
                s.map_or_else(
                    || "– | – | – | –".to_string(),
                    |s| {
                        format!(
                            "{} | {} | {} | {}",
                            ms(s.median),
                            ms(s.p90),
                            ms(s.p99),
                            ms(s.max)
                        )
                    },
                )
            };
            let first = if i == 0 {
                "–".to_string()
            } else {
                ms(self.first.0[i])
            };
            let _ = writeln!(
                out,
                "| `{phase}` | {} | {first} | {} |",
                cells(fresh),
                cells(repeated)
            );
        }
        if let Some(s) = summarize(self.wall) {
            let _ = writeln!(
                out,
                "| process wall (spawn to exit) | {} | {} | {} | {} | – | – | – | – | – |",
                ms(s.median),
                ms(s.p90),
                ms(s.p99),
                ms(s.max)
            );
        }
        out
    }
}
