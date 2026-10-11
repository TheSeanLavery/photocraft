# Upstream integration, 2026-10-11

This merge integrates upstream `storytold/photocraft` main
`5f55a29d2bb0601edbaabe7f2f708c8e51d6439c` into fork main
`611b78373405c228e2b2d02a3ff5b727e78d01bb`. The merge base is
`bdd0141cf31b23c32a95d891e67990e462b212d2`: 101 fork-only commits,
1005 upstream-only commits and 152 initially conflicted files. Seven newer upstream
commits were reviewed and integrated in a follow-up merge with 15 conflicts.

Three agents reviewed core/history/collaboration, UI/platform behavior and
formats/rendering. After resolving their assigned files, they independently
reviewed each other's resolutions. The measured acceptance results are below.

## Preservation decisions

| Fork feature | Combined behavior |
| --- | --- |
| Branching and unlimited history | Keep archived checkpoints, stable history IDs, alternative branches, scrubber previews and compact moves. Add upstream selected-layer restoration and bounded undo damage; new history commands hydrate cold destinations atomically. |
| Collaboration and author undo | Keep private author tool contexts, prediction and room transport. Add symmetry and mask colors to author contexts; publish complete mirrored strokes. Cross-document copies use the destination document's sharing state. Stored brush resources resolve on their owning artist and replay portably on peers. |
| Crash recovery | Keep complete document/history/navigation checkpoints, ownership locks and asynchronous retirement. Upstream deferred discovery and cancellable jobs load complete fork checkpoints. |
| HDR | Keep extended-sRGB presentation, float painting/pickers, workbench, previews and exports. Carry HDR gallery buffers through upstream asynchronous rendering; reserve distinct shader uniform slots for HDR and rotation. Shell-only colors stay SDR. |
| Themes and numeric fields | Keep runtime packs, live reload, historical Win98/Mac OS 9.1/System 6 geometry and fonts, floating tools and arithmetic fields. Combine the expanded built-in palette catalog and shared docking/widgets. Theme persistence failures roll back selection. |
| File formats | Keep every writable fork codec and expose all readable codecs in file pickers. Add upstream Affinity, SVG/OpenRaster, Paint.NET, FITS, EXR and RAW improvements. Rich OpenRaster stacks retain safe composite fallback and color profiles. |
| Input and automation | Keep Force Touch, coalesced mouse input, Pencil/Pen options and ordered control strokes. Add upstream pressure curves, symmetry, rotation and picker controls. Install macOS event monitors after AppKit initialization. |
| Distribution | Keep fork filmstrip branding, separate CLI, symbols, signing verification and native-release profile. Add upstream installer/layout/platform fixes and served browser fonts. Preserve immutable framework patch pins and the 24 MiB browser packaging gate. |

Upstream deliberately changes stepped Alt-scroll zoom, permits overscroll when
an image fits, adds Color Replacement to the B shortcut cycle, follows OS DPI
and raises maximum zoom to 12800%. Quick Selection adopts upstream selection
extent behavior; edge barriers and bounded coarse-to-fine processing retain
dedicated regressions.

## Validation

Validation used an isolated worktree and settings directories on an Apple M1 Max.
The occupied historical-theme checkout and its uncommitted work were preserved.
Counts below describe individual lanes; they overlap and must not be added into
a unique repository-test total. Initial failures were corrected and their affected
targets rerun before acceptance.

| Gate | Measured result |
| --- | --- |
| Pinned real-file corpus | 2,973 passed, 0 failed, 27 ignored across 130 reported targets, with `corpus,heif`. Pins verified before execution. Release tests used the tracked CI profile's no-LTO configuration; performance measurements used the actual release profile. |
| Low-level correction rerun | 785 passed, 0 failed, 9 ignored across 57 targets, including compose, I/O and text. Other initial low-level targets passed, including algorithm, native format/recovery, GPU parity, RAW, raster and tablet. |
| Engine | 1,390 unit cases and 4 preference-use audit cases passed. Includes unlimited/cold history, author-private collaboration resources, HDR and all-depth editing. |
| Final UI | 2,279 library cases passed, 15 ignored; full-resolution preview 3 passed; historical chrome 9 passed. Includes raw-input ownership, peak-pressure tap and exactly-once pressure-curve regressions. |
| Automation | 58 library and 37 MCP cases passed; installed-profile requests reject implicit ambient filesystem scans. |
| Native app | 119 cases passed in the broad run; the sole retry-timing failure passed its focused correction run (30.07 s). The retry test now accounts for the existing 30 s backoff and async completion. |
| Adversarial commands | Explicit ignored `panic_hunt` invocation passed. |
| Tooling and packaging | All 59 xtask cases passed, including 6 asset-packer cases; 7 Python packaging regressions passed; workspace doctests passed (1 executable case across 27 reported targets). |
| Static gates | Formatting and diff checks passed; strict workspace Clippy with `--locked --features heif --all-targets -D warnings` passed; 31 crates passed layering; all 25 WASM-safe crate checks passed and both browser binaries built. |
| Catalogs | All 17 Fluent catalogs validate; all 17 registry catalogs, including English, cover 1,772/1,772 source IDs. Authored compressed locale bytes round-trip exactly. Menu dispatch remains 631/631; this measures wiring, not full Photoshop behavior. Generated scorecard is current. |

The source-preservation inventory checked 1,966 test names in fork-touched files.
This is a name audit, not an executed-test count or a claim that every repository
test was inventoried. Missing names were restored or mapped to moved/expanded
tests and the intentional upstream behavior changes described above.

Hosted Windows packaging exposed a pre-existing setup omission: ARM64 binaries
compiled and passed PE checks, but WiX could not find its required UI extension.
The native-package, ARM64 and signed-release workflows now install both UI and
utility extensions at the same pinned WiX 5.0.2 version, checking every setup
command's exit code. A contract regression checks every Windows packager's
extension requirements and fail-fast setup. Payload, signing and separate CLI
packaging remain unchanged. Hosted gate results are recorded on the integration
pull request.

### Runtime and visual acceptance

- Four isolated native artists completed 18 overlapping strokes each, independent
  layer creation and personal undo/redo. All reached revision 668 with identical
  exported RGBA pixels, empty queues and no errors. The decoded pixel SHA256 was
  `00e33fb138e9a4c42259864bf69a123cf27141fafd971470e18bc98e5ab73dba`.
- A browser host served under `/nested/app/` accepted four native peers and all
  32 additional overlapping strokes. Native peers converged at revision 292.
  A stroke painted through the browser reached every peer at revision 303;
  browser personal undo restored the exact prior pixels at revision 304, and
  redo restored the stroke exactly at revision 305. Browser worker transport ran
  during these operations. Signaling used a same-origin loopback proxy to the
  local room service; public WAN/forced TURN was not part of this check.
- Win98, Mac OS 9.1 Platinum and System 6 runtime packs passed at 1x, 1.5x and 2x
  on CPU and GPU. All six runs passed live reload, invalid-file retention, atomic
  recovery and deleted-file retention, with 36 captures inspected.
- Five HDR preview modes produced framebuffer and EXR evidence. HDR and split
  modes retained approximately 3.997x radiance; SDR preview modes mapped to
  approximately 1.004x. This used simulated 4x display headroom and proves float
  output, not physical screen brightness.
- Browser startup, preset document creation, painting, undo and redo passed in
  Chrome. Generic WASM MIME type used the buffered fallback successfully. Missing
  assets, corrupted executable and corrupted assets left startup disabled.

### Browser packaging

The combined browser initially exceeded the existing 24 MiB per-file limit.
Lossless localization compression and size optimization preserve every locale;
the six hot pixel-processing crates retain optimization level 3. A Rust xtask
post-build pass separates active static data (fonts, strings, shaders and tables)
from executable functions, retaining offsets, data indices, tables and globals.
It adjusts the generated wasm-bindgen loader; the editor and transport remain
Rust/WASM, with no JavaScript UI framework or separately authored JavaScript app.

The executable and data module share an original-module digest marker. Both are
checked before instantiation; data initializes the executable's exported memory
before explicit Rust startup. Final filenames use content hashes and both fetches
use SRI. Staged publication rolls back on failure and rejects unrecognized
compiler output. The package cap remains 25,165,824 bytes for every runtime file,
including optional served fonts.

| Final file | Bytes |
| --- | ---: |
| Executable WASM | 22,787,987 |
| Static assets WASM | 7,046,425 |
| Collaboration codec worker WASM | 3,256,221 |

The exact final optimized original module and packed pair reconstructed all
8,585,216 initial memory bytes identically, with zero pre-start import calls.
Memory SHA256: `493c6d23b54d494b5e1da62e1f04d462e2053d4e064b79c760460c7465c8a736`.
Twelve emitted-loader acceptance gates passed, including async/sync initialization,
nested paths, generic MIME, missing/corrupt inputs and mismatched pair rejection
before callbacks. Packed-site repeat validation and release ZIP packaging passed.
Chrome's existing 8 MiB synchronous-instantiation limit means the production
browser uses asynchronous initialization; the preloaded sync contract is tested
with a bounded fixture. Browser IndexedDB preserves custom brushes; document
autosave/recovery remains a native service.

### Performance and remaining scope

A release 36.2 MP (7360x4912) benchmark with three repetitions completed: median
brush dab 2.8 ms, 40-dab stroke 20.5 ms, undo 5.2 ms, two-layer full refresh 7.8 ms,
CPU viewport 36.4 ms, JPEG open plus document creation/refresh 1,052 ms and PNG
export 3,950 ms. These are smoke
measurements under concurrent build load, not a before/after comparison.
The final quick performance suite ran five benchmark commands with no command
failures. Its report covers 56 inventory entries: 43 measured, eight not run
in quick mode and five not yet measurable.
The checked-in M4 Pro baseline differs from this M1 Max, so regression comparison
is skipped. Some heavy layout operations exceed 16 ms; no new performance-baseline
or all-operations-within-budget claim is made.

Physical tablet/Force Touch feel and public WAN/forced-TURN sessions require
separate hardware/network evidence. Installer execution on other platforms is
checked separately by hosted workflows; the local macOS acceptance does not
prove it. These limits do not replace the passing source, corpus and loopback
runtime checks.
