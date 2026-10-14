# PSD loading performance work

Tracking issue: https://github.com/storytold/photocraft/issues/5392.

Implementation proceeds sequentially on one branch. Keep each measured checkpoint so that
later parser, upload and composition changes can be compared against the immediately prior
version. Generated PSDs and images are local fixtures, not committed assets.

## Validated checkpoints

The Linux experiment uses immutable 7360×4912 three-layer and complex eight-layer PSDs
created from an original generated 1536×1024 image, scaled deterministically. The three-layer
file's BLAKE3 is `497008838e0cd18506e4b2e0cf533ff001f53fe22d1bbc1a9aab41d90df8de0b`.

Ryzen 9 7950X3D / RTX 3090, Vulkan, Rust 1.99, release opt-level 3, LTO off,
16 codegen units, `-C target-cpu=native` for both reference and candidate. The fixed-file
harness times in-memory import, Session admission, full-canvas GPU encode/submit and wait.
Disk, GPU initialization, compositor construction, readback and validation are excluded.
The compositor is fresh per sample. Alternate and reverse version order; keep raw samples.
These are P44-class workloads, not the original CI P44 file or native UI startup measurements.

The prior import prototype reduced measured three-layer opening from 621 to 250 ms and
complex opening from 287 to 136 ms. That historical comparison is separate from subsequent
adjacent comparisons; do not combine batches to attribute a gain to a single change.

| Sequential step | Three-layer reference → candidate | Evidence |
|---|---:|---|
| Borrow native tile bytes during GPU upload | 251.3 → 236.0 ms | `perf/experiments/psd-open-linux-step1.json` |

The direct upload step removes temporary copies for native RGBA8/GRAY8 formats while
preserving shared-tile grouping and ordered uploads. CPU preparation changed from 87.3 to
71.1 ms in the paired three-layer run. The complex run was noisy (136.0 → 134.8 ms overall);
no precise complex speedup is claimed for this step.

## Correctness

The retained import prototype passed 35 PSD compression tests and three focused import
integration tests, including malformed channels, deterministic warning order, PSD/PSB,
8/16/32-bit samples, negative bounds, masks and multi-depth round trips. Complete imported
raster/mask/structure signatures match both fixed fixtures. Four representative GPU crops
per fixture pass the CPU oracle within one 8-bit level plus floating-point tolerance; this
is not full-frame GPU bitwise equality. The direct-upload shared-tile/multi-format unit test
also passed on Linux.

The parallel RLE path validates packets in original row order before initializing the full
output; removing that validation caused a malformed-file memory regression in a rejected
trial. Worker-count tuning and fixed packing blocks were rejected because they did not
produce repeatable useful improvements.

## Remaining work

The umbrella issue records parser borrowing, pass fusion, adaptive cache population,
mask/flattened conversion redundancies, other sample formats, Navigator/mips, sparse first
view and lazy materialization. None of those is claimed complete by this checkpoint.
Broad corpus, full lint/wasm checks and native UI acceptance are pending. This document
must be updated as each sequential step is implemented and validated.
