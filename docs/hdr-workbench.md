# HDR Workbench

Open **Window → HDR Workbench** with a 32-bit document. On a supported macOS
EDR surface, HDR Output retains highlights up to the live display headroom.
Turn HDR Output off to preview SDR without changing the document. Other
platforms and CPU fallback show SDR. Reference white is 1.0; +1 EV is twice
white and +2 EV is four times white. No uncalibrated nit values are displayed.

## Display controls

- **Natural** leaves values below the shoulder unchanged and compresses brighter
  values smoothly toward SDR white. This is the default.
- **Filmic** adds a soft shadow toe to the same highlight shoulder.
- **Clip** clips channels above white, useful for comparison and diagnosis.
- **Exposure**, **Contrast**, and **Saturation** grade the display in linear RGB.
  **Shoulder start** sets where SDR highlight compression begins.
- **Split preview** shows HDR on the left and the selected SDR transform on the
  right. If display headroom is 1, both sides have SDR range.
- **Clipping stripes** mark colors exceeding the output headroom before highlight
  compression. They diagnose the graded display signal, not a permanent pixel edit.

The histogram samples a nearest-neighbor proxy, uses peak RGB channel in linear
sRGB, and spans −8 to +8 EV. White and live display headroom are marked. This
is a sampled distribution, not a full-resolution clipping count; tiny highlights
can be missed. It is cached by document revision and profile. Moving a preview
slider does not recompose the document or rebuild the histogram.

These are original display operators, not ACES output transforms. Full ACES 2
and Resolve-style shadow/highlight/specular zone grading remain later work.

## Float colors

In a 32-bit document the foreground/background and gradient-stop picker stores
float RGB arrays instead of hex colors. Its numeric fields use the document's
composite RGB profile, which is named in the dialog. RGB gain doubles the numbers
per EV; this is a light-intensity change in a linear working profile. The hue
field is SDR, while the new/current swatches render through the float GPU canvas.
Their visible brightness is limited by current display headroom.

The eyedropper reads original composite pixels, including values above 1.0;
paint writes them to float layers. Integer layers necessarily quantize. Saved
HDR swatches are persisted in preferences as linear-sRGB floats and converted
through ICC transforms into the current document's composite profile on recall.
Up to 32 swatches can be stored. `tools.setColors` validates both colors before
changing either, rejects malformed input, and accepts hex or float arrays.

## Matching SDR export

**File → Export → Export SDR Preview…** (also available in the workbench) writes
an explicit, flattened sRGB PNG with the same Natural/Filmic/Clip display grading.
Original float pixels and normal HDR/EXR export stay available. Comparison lines,
clipping stripes, and the histogram are not baked into the export. The exporter
currently supports up to 36 MP. Disable Proof Colors and reset legacy 32-bit
Preview Options before matching export; otherwise an actionable error prevents
an ambiguous result. Use the workbench exposure control for this workflow.

Automation:

```json
{"command":"view.hdrWorkbench","params":{"method":"natural","exposure":0,"shoulder":0.75,"contrast":1,"saturation":1,"split":true,"clipping":false}}
{"command":"view.hdrDiagnostics","params":{}}
{"command":"tools.hdrSwatches","params":{"action":"save"}}
{"command":"tools.hdrSwatches","params":{"action":"recall","index":0}}
```

CLI engine export: `file.export.sdrPreview` with an explicit `.png` path. Desktop
control export: `app.exportSdr {"path":"preview.png"}` writes only beneath the
authorized automation write root; it never opens a save dialog or uses ambient
filesystem authority. Open the panel through `ui.menu.invoke` with id
`window.hdrWorkbench`. `ui.set {"hdrOutput":false}` selects SDR presentation.

## Validation and limits

The float framebuffer test compares all three GPU operators with the CPU
reference, including Display P3 and Adobe RGB-compatible profiles, exercises
exposure/contrast/saturation and split preview, and checks transparency. Engine tests cover malformed command input, float paint and pick,
`.pcraft` round trips, persistent swatches and embedded sRGB export.

The canvas uses the existing extended ICC LUT convention: normalize HDR into its
bounded LUT domain, transform the color, then restore its radiance. The CPU
reference uses the same convention. This is not an unbounded spectral or full
ACES color-management pipeline. GPU LUT interpolation and half-float rounding
can differ slightly from CPU output. The CPU fallback maps before 8-bit
quantization; documents exceeding the float canvas budget no longer silently
clip HDR before the SDR operator. Native EDR transport still depends on the
pinned egui-wgpu fork described in `patches/egui-wgpu-hdr.md`.

Reproduce controlled offscreen captures (Metal on macOS, no desktop focus needed):

```sh
cargo run -p photocraft --example hdr_fixture -- /tmp/hdr-chart.exr
cargo run -p photocraft-ui-egui --example hdr_snapshot -- \
  --open /tmp/hdr-chart.exr --out /tmp/hdr-workbench --mode split
```

The second command writes a clipped PNG preview and a linear-sRGB float EXR of
the full editor framebuffer. It explicitly simulates 4× headroom; it does not
measure physical brightness or pretend to be a native-window capture. Modes:
`hdr`, `sdr`, `filmic`, `clip`, `clipping`, `split`; add `--picker` to show the float
picker. Native window capture still uses `ui.screenshot` plus
`PHOTOCRAFT_HDR_CAPTURE_DIR` from the HDR output PR. CPU transform timings:
`cargo run --release -p photocraft-engine --example hdr_bench` (24 MP).

On the M1 Max, a release 24 MP CPU benchmark measured the previous clipping-only
conversion at 111–120 ms and Natural SDR conversion plus quantization at
328–349 ms (three runs). This is conversion/export cost, not frame rate. GPU
preview performs these controls in its display shader; CPU preview caches the
converted image until its document or view settings change.
