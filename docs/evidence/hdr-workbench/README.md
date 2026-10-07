# HDR workbench evidence

Synthetic 960×540 float EXR chart: reference-white patches 0, 0.18, 1, 2, 4 and neutral/warm ramps. No personal photographs.

Offscreen Metal captures use a float framebuffer and explicitly simulated 4× headroom. PNG previews clip at SDR white; accompanying EXRs preserve the linear-sRGB framebuffer. These demonstrate rendering and controls, not measured display brightness.

Native captures are listed separately, using the connected Mac M1 Max XDR display's reported live headroom. SDR export excludes UI, clipping stripes and the split divider.

Reproduction and limitations: [HDR Workbench](../../hdr-workbench.md).

## Native views

- [HDR enabled](native-hdr.png), [SDR Natural](native-sdr.png), [Filmic](native-filmic.png), [Clip](native-clip.png)
- [Split preview](native-split.png), [clipping stripes](native-clipping.png), [float picker](native-picker.png)
- [Matching SDR export](matching-sdr.png)

The connected display reported 9.10–10.32× headroom (+3.19 to +3.37 EV) during validation. Headroom varies with display brightness and system state. Float readbacks are scene-linear sRGB; PNG captures cannot demonstrate physical HDR brightness.

## Checks

Affected-crate tests: 1,253 passed, 16 opt-in tests ignored; the separate ignored adversarial command test passed. The final UI unit pass also passed all 557 tests. Float GPU comparison covers all three operators, exposure/contrast/saturation, split, alpha, Display P3 and Adobe RGB-compatible profiles. Layer, wasm, parity and scorecard checks pass.

Strict Clippy remains blocked by pre-existing `collapsible_match` in `crates/plugins/src/manifest.rs:125`. The touched crates pass `--all-targets --no-deps -D warnings` with only the existing `collapsible_match`, `nonminimal_bool` and `manual_range_contains` categories allowed on the command line; no lint allowances were committed.

Release 24 MP CPU benchmark: clipping-only conversion 111–120 ms; Natural SDR conversion plus quantization 328–349 ms (three runs). This measures conversion/export, not FPS. GPU controls execute in the display shader and CPU preview is cached by document and view settings.

Native float captures: [HDR EXR](native-hdr.exr), [SDR EXR](native-sdr.exr); [numeric measurements](measurements.json). Controlled offscreen captures: [split](split.png) / [EXR](split.exr), [picker](picker.png) / [EXR](picker.exr).

`cargo xtask perf --quick` completed successfully. It continues to report over-budget general layout/compositing cases; this change does not claim those existing budgets are solved.
