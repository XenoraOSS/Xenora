# SPDX-License-Identifier: GPL-3.0-or-later
"""Benchmark result analysis over existing measurement samples.

Consumes numeric samples only. Does not run workloads or invent timings.
"""
from __future__ import annotations

import json
import math
import statistics
from dataclasses import asdict, dataclass
from typing import Any, Sequence


@dataclass(frozen=True)
class SampleStats:
    count: int
    median: float
    p95: float
    p99: float
    mean: float
    stdev: float
    iqr: float
    minimum: float
    maximum: float


@dataclass(frozen=True)
class RegressionResult:
    metric: str
    baseline_median: float
    current_median: float
    ratio: float
    threshold: float
    regressing: bool


def _percentile(sorted_samples: Sequence[float], pct: float) -> float:
    if not sorted_samples:
        raise ValueError("no samples")
    if pct <= 0:
        return float(sorted_samples[0])
    if pct >= 100:
        return float(sorted_samples[-1])
    k = (len(sorted_samples) - 1) * (pct / 100.0)
    f = math.floor(k)
    c = math.ceil(k)
    if f == c:
        return float(sorted_samples[int(k)])
    d0 = sorted_samples[f] * (c - k)
    d1 = sorted_samples[c] * (k - f)
    return float(d0 + d1)


def summarize(samples: Sequence[float]) -> SampleStats:
    if not samples:
        raise ValueError("summarize requires at least one sample")
    ordered = sorted(float(x) for x in samples)
    if any(not math.isfinite(x) for x in ordered):
        raise ValueError("benchmark samples must be finite")
    q1 = _percentile(ordered, 25)
    q3 = _percentile(ordered, 75)
    stdev = statistics.pstdev(ordered) if len(ordered) > 1 else 0.0
    return SampleStats(
        count=len(ordered),
        median=statistics.median(ordered),
        p95=_percentile(ordered, 95),
        p99=_percentile(ordered, 99),
        mean=statistics.fmean(ordered),
        stdev=stdev,
        iqr=q3 - q1,
        minimum=ordered[0],
        maximum=ordered[-1],
    )


def compare_medians(
    baseline: Sequence[float],
    current: Sequence[float],
    *,
    metric: str = "latency",
    threshold: float = 1.10,
) -> RegressionResult:
    base = summarize(baseline)
    cur = summarize(current)
    if base.median <= 0:
        ratio = math.inf if cur.median > 0 else 1.0
    else:
        ratio = cur.median / base.median
    return RegressionResult(
        metric=metric,
        baseline_median=base.median,
        current_median=cur.median,
        ratio=ratio,
        threshold=threshold,
        regressing=ratio > threshold,
    )


def export_json(
    stats: SampleStats,
    *,
    host: str,
    toolchain: str,
    revision: str,
    metric: str,
) -> str:
    payload: dict[str, Any] = {
        "metric": metric,
        "host": host,
        "toolchain": toolchain,
        "revision": revision,
        "stats": asdict(stats),
    }
    return json.dumps(payload, indent=2, sort_keys=True) + "\n"


def human_report(stats: SampleStats, *, metric: str) -> str:
    return (
        f"{metric}: n={stats.count} median={stats.median:.6g} "
        f"p95={stats.p95:.6g} p99={stats.p99:.6g} "
        f"iqr={stats.iqr:.6g} stdev={stats.stdev:.6g} "
        f"min={stats.minimum:.6g} max={stats.maximum:.6g}\n"
    )
