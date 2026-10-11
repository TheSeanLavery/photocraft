# Upstream integration, 2026-10-11

This merge integrates upstream `storytold/photocraft` main
`5f55a29d2bb0601edbaabe7f2f708c8e51d6439c` into fork main
`611b78373405c228e2b2d02a3ff5b727e78d01bb`. The merge base is
`bdd0141cf31b23c32a95d891e67990e462b212d2`: 101 fork-only commits,
1005 upstream-only commits and 152 initially conflicted files. Seven newer upstream
commits were reviewed and integrated in a follow-up merge with 15 conflicts.

Three agents reviewed core/history/collaboration, UI/platform behavior and
formats/rendering. After resolving their assigned files, they independently
reviewed each other's resolutions. Validation is recorded below when complete.

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

Validation is in progress. This document must be completed with measured results
before merging into fork main. Physical tablet/Force Touch feel and public WAN
or forced-TURN sessions require separate hardware/network evidence.
