# Selective upstream imports

Source repository: https://github.com/ranveersoni98/Xenora
Upstream main inspected at76b0ed5 (25 November2026). This fork does not claim to
contain all subsequent upstream changes.

## Compute pipeline cache

Adapted from commit6f1ac2a906ff817f4d933c799cd1f8fa4822cbf9, "Reuse Metal compute
pipelines", authored by katherinebytes <rebooteduck+kr@gmail.com>.
Original commit carries Signed-off-by: katherinebytes <rebooteduck+kr@gmail.com>.
The imported compute.rs and native_compute.rs code is part of xenora-metal, whose
Cargo.toml inherits workspace GPL-3.0-or-later licensing; LICENSE and workspace
metadata at that exact commit were checked. No additional file-level license or
copyright notice was present. Existing repository licensing remains in force.

Modified files: compute.rs, render_cohort.rs (initialize each new context cache),
and tests/native/pipeline_cache.rs (tests extracted from
upstream native_compute.rs). Adaptation keeps our synchronous execution and existing
compute_pipeline/conditional shader-validation helpers used by gather and patch
kernels. It imports bounded per-context source/entry/validation-mode caching and
adapts token-based tests to synchronous calls. It does not import asynchronous
submission retirement, buffer recycling or the upstream benchmark. No upstream
performance measurements are claimed for this fork.

Other reviewed changes are deferred: eb85731 asynchronous compute retirement and
4f3d0ae buffer recycling require a combined ownership/lifetime integration;
a6b0e89 packed BC chains overlaps the existing canonical layout. Upstream's expanded
guest graphics ABI also requires reconciliation before a full merge. These are
integration decisions, not claims that the upstream changes are defective.
