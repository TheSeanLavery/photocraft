# Runtime theme files

PhotoCraft accepts runtime theme files in native debug and release builds. Set
`PHOTOCRAFT_THEME_FILE` to a JSON file before launching the app. Saving changes updates the
running app without recompiling. The entire definition is validated before the UI changes;
invalid or incomplete saves retain the last working appearance and produce a status message.

```json
{
  "version": 1,
  "id": "custom.silver",
  "name": "Custom Silver",
  "base": "classic",
  "colors": {
    "chrome": "#C0C0C0",
    "card": "#C0C0C0",
    "field": "#FFFFFF",
    "accent": "#000080"
  },
  "geometry": {
    "radiusSmall": 0,
    "radius": 0,
    "radiusLarge": 0,
    "bevelWidth": 2,
    "itemSpacing": [6, 4],
    "buttonPadding": [8, 3],
    "controlSize": [20, 20],
    "buttonHeight": 23,
    "fieldHeight": 20,
    "tabHeight": 22,
    "panelPadding": 4,
    "panelGap": 3,
    "rowGap": 3,
    "titleBarHeight": 26,
    "optionsBarHeight": 30,
    "statusBarHeight": 24,
    "toolButtonSize": 26,
    "toolbarPadding": 4,
    "railButtonSize": 24,
    "preferenceSpacing": [8, 4],
    "menuMargin": 2,
    "dialogMargin": 10
  },
  "typography": {
    "small": 10,
    "body": 12,
    "button": 12,
    "heading": 13,
    "monospace": 11
  }
}
```

This example tunes the existing Classic theme. It is not a complete historical Windows 98
reproduction; dedicated historical control rendering is still being built.

- `version`: currently `1`. Unknown versions and unknown fields are rejected.
- `id`: stable ID, 1–64 ASCII letters, digits, dots, underscores or hyphens.
- `name`: display name, 1–128 bytes of printable text.
- `base`: `pro`, `proMedium`, `studio`, `studioLight` or `classic`. Missing overrides inherit
  the built-in base. Packs cannot inherit other packs.
- `appearance`: `modern` (default), `windows98`, `platinum`, or `system6`. This selects
  the app-drawn control renderer independently of `base`. Older packs omit it and keep the
  existing renderer.
- `colors`: semantic `Tokens` color names, with strict `#RRGGBB` or `#RRGGBBAA` values.
  Available names are `chrome`, `canvas`, `canvas_dot`, `dock`, `card`, `card_border`, `field`,
  `field_border`, `hover`, `pressed`, `text`, `text_dim`, `text_faint`, `icon`, `accent`,
  `accent_soft`, `accent_border`, `accent_text`, `separator`, `shadow`, `primary_bg`,
  `primary_text`, `danger`, `warning`, `tab_strip`, `row_selected`, `bevel_highlight`,
  `bevel_light`, `bevel_shadow`, and `bevel_dark`.
- `geometry`: radii 0–32 points; `itemSpacing` and `buttonPadding` pairs 0–64 points;
  `controlSize` pair 8–128 points; `sliderWidth` and `dropdownWidth` 16–1024 points;
  `iconWidth` 8–64 points. `bevelWidth` accepts 1–2; 2 draws nested raised/sunken edges
  in shared bevel surfaces, while 1 preserves the existing Classic appearance.
- App-drawn geometry: `buttonHeight`, `fieldHeight`, `tabHeight`, `titleBarHeight`, `menuBarHeight`, `documentTabHeight`,
  `optionsBarHeight`, `statusBarHeight`, `toolButtonSize`, `railButtonSize` accept 16–96 points.
  `panelPadding`, `panelGap`, `rowGap`, `toolbarPadding`, `menuMargin`, `dialogMargin` accept
  0–32 points; `preferenceSpacing` is a horizontal/vertical pair, each 0–32 points.
  `buttonPadding` also controls horizontal padding in custom buttons. Defaults preserve existing
  themes. Buttons, numeric fields and tabs grow when needed to fit the configured type size.
  Historical bars also grow to contain their fields and labels. `layerRowHeight` accepts
  24–96 points; `layerThumbnailSize` accepts 8–64 and is capped to fit the row with a
  four-point border allowance. `swatchSize` accepts 8–48 points and lays out a compact
  wrapping grid; omitted, the existing automatic grid is preserved. `colorPanelHeight`
  accepts 80–600 points and supplies the initial Color group height without overwriting
  user-resized heights or saving preview dimensions.
- `typography.family`: optional registered UI family: `Inter`, `JetBrains Mono`,
  `Microsoft Sans Serif`, `Geneva`, `Chicago`, `Charcoal`, or `Chicago Kare`. Native builds read these faces
  from known installed-font locations, with bounded reads and parsing; no font files enter
  this repository. The built-in Inter stack remains the fallback. Missing Chicago uses Chicago Kare when configured, then installed Geneva, then Inter.
  Missing Charcoal uses installed Geneva when available, then Inter. The manager reports substitutions;
  `ui.inspect.themeFont` exposes requested and actual family. Browser builds use the registered
  fallback stack when historical faces are unavailable. Current family registrations are cached
  for the app session; restart after installing an additional face.
- `typography.caption`: app-drawn historical app, dialog and floating-window title size,
  6–64 points (default 12). Caption height grows only when needed to contain the text.
- `typography`: egui text-style sizes, 6–64 points; `button`, `monospace` and `body` also size
  shared custom button text, numeric fields and pill tabs. Current named font families and other
  explicitly sized custom widgets retain their existing fonts and sizes.

Files are limited to 64 KiB. Numbers must be finite and within the documented ranges. The
loader keeps working across ordinary editor saves, including temporary invalid JSON and
atomic file replacement. Native builds read and validate the active file on a background
worker every two seconds, comparing its contents rather than timestamps. Frame updates
consume a single latest result without waiting for the worker. The first override arrives
asynchronously; malformed or deleted files retain the last applied appearance.

Legacy flat token files continue to work. They inherit the currently selected built-in theme:

```json
{ "card": "#323232", "accent": "#378EF0", "radius": 4 }
```

Legacy numeric keys are `radius_sm`, `radius`, and `radius_lg`; all other keys must be known
color tokens. Their values undergo the same validation as versioned packs.

## Saved theme library

Custom packs have stable IDs independent of their display names. Native builds store the
validated library at `Themes/library.json` beneath the existing configuration directory,
including portable mode. The web app uses `photocraft.themes` in browser local storage.
Libraries are limited to 64 custom packs and 4 MiB. Built-in IDs (`builtin.*`) are reserved.
Native builds also watch this library in the background. External valid edits update the
selected theme, including edits that retain the same ID; malformed or deleted libraries
retain the last applied catalog. Local atomic saves invalidate earlier pending reads.
Theme Manager previews defer external updates until the preview ends, and an open editor
draft remains intact. Browser imports and commands update the catalog directly; changes
from another browser tab are not watched.

`interface.themeId` stores the selected ID. Existing preferences with only `interface.theme`
are normalized from that built-in choice during loading. The original enum remains the
fallback. A missing selected pack uses that fallback, retains the requested ID, and recovers
if the pack becomes available again. Selecting a legacy enum through `prefs.set` still works.

Preferences → Interface → Theme lists built-ins and imported themes by name. Agent commands
can import and select packs; see [Control protocol](control-protocol.md). Importing an existing
ID requires `replace:true`. Failed library saves preserve the in-memory catalog; failed
selection saves preserve the working appearance and preference. Without platform storage
services (for example in an offscreen harness), changes are session-only and command responses
report `persisted:false`.

The environment-variable file adapter is native-only and applies after the selected library
theme as an explicit live override outside a manager preview. Historical rendering and font
fallbacks are described below. Release and browser runtime acceptance remain in progress.


## Theme Manager

Open **Window › Theme › Theme Manager…**, or find it in the command palette. Choose a theme,
then **Duplicate** to edit a built-in under a custom ID. The definition editor accepts the same
versioned JSON used by runtime files. **Preview** validates the draft and changes the app's
appearance without changing saved preferences or the theme catalog. Invalid edits keep the
last valid preview. **Apply** saves the validated definition and selected ID; **Cancel**, Escape,
or the window close button restores the saved theme. **Reset Changes** reloads the selected
catalog definition. Switching to another catalog entry discards the draft and preview.

**Import…** opens a JSON picker and loads a validated draft; Apply stores it. **Export…** saves
or downloads a portable `.theme.json` file. **Copy JSON** puts the editor text on the clipboard.
Native reads are bounded to 64 KiB. Browser imports check size before reading, handle read
errors, and deliver their result asynchronously. A file picker cancellation changes nothing.

The manager reports persistence errors and keeps the editor open. If saving the selected ID
fails after writing the catalog, it rolls back the catalog write; a rollback failure is reported
explicitly. Built-in definitions cannot be overwritten. Importing an existing custom ID and
pressing Apply replaces that custom definition. Commands taking parameters remain available
to agents, while the command palette exposes the interactive manager and built-in selectors.


## Historical preset development

Window › Theme and Theme Manager include **Windows 98 SE**, **Mac OS 9.1 Platinum**, and
**Macintosh System 6**. Portable definitions live in `assets/themes/`; export/duplicate any
preset to edit it without recompiling. Stable IDs are `builtin.windows-98-se`,
`builtin.mac-os-9-1`, and `builtin.system-6`.

The first renderer pass differentiates button/default/focus states, checkbox and radio shapes and marks,
panel tab borders, sunken fields, dropdown arrows, toolbar selection, and app-drawn main/modal captions. Windows
uses square bevels and an active caption gradient; Platinum uses rounded buttons and striped
captions; System 6 uses monochrome chrome and X-mark boxes. Native outer decorations remain
host-managed. Document pixels, swatch values and other color content retain their actual colors.

These presets are still under historical acceptance. Exact bitmap typography, all custom
widget fonts/metrics, complete disabled-state treatment,
remaining floating-window captions, and complete multi-scale control-state comparisons are
not finished. System 6 panel tabs adapt PhotoCraft's docking UI to monochrome; System 6 did not have this modern docking workflow.
Custom disabled button/check/radio ink uses a checker mask. Microsoft Sans Serif is a metrically compatible vector successor, not Windows
98's MS Sans Serif bitmap face. Chicago and Charcoal are not present on the current machine;
Geneva is the reported fallback. Do not describe this first pass as pixel-perfect reproduction.

Clean-room references: [Microsoft shell font history](https://learn.microsoft.com/en-us/windows/win32/Intl/using-ms-shell-dlg-and-ms-shell-dlg-2),
[Microsoft Sans Serif metric compatibility](https://learn.microsoft.com/en-us/typography/font-list/microsoft-sans-serif),
[Apple-authored Platinum controls and layout guidelines (archived)](https://dev.os9.ca/techpubs/mac/pdf/HIGOS8Guidelines.pdf),
and [Apple's Platinum typography note (archived)](https://dev.os9.ca/techpubs/mac/HIGOS8Guide/thig-8.html).

For visual review without opening an editing session, run
`cargo run -p photocraft-ui-egui --example historical_controls -- <slug> <scale> <state> <output.png>`.
Slugs are `windows-98-se`, `mac-os-9-1`, `system-6`; states are `normal`, `hover`,
`pressed`, and `focus`. The gallery exercises production control painters and retains
the true document-color swatches.

Historical scroll areas and the canvas use end arrows, press-and-hold line scrolling,
page clicks, and draggable thumbs. Windows 98 uses a proportional scroll box; the
classic Mac profiles use a fixed scroll box (a historically available style). Platinum
uses the configured accent for its scroll box, while System 6 uses black-and-white
checker tracks and hollow arrows. Canvas bars remain outlined when the image fits;
panels display bars when their contents overflow. Wheel input and virtualized rows
remain managed by egui. Historical canvas space reserves the bar region before zoom,
fit and tool input, so scrolling cannot paint beneath a scrollbar.

`geometry.scrollBarWidth` (12–32 logical points) adjusts app scrollbar width without
recompiling. The three historical definitions default to 16 points. Gallery states
`arrow` and `scroll` show a held arrow and the resulting scrolled content.


## Offscreen release reload acceptance

The `runtime_theme_acceptance` example exercises the production frame loop and native
environment-file watcher in a release build, without opening a window or using user settings.
It loads all three historical packs, checks malformed-file retention, atomically replaces the
file with changed colors and button height, and verifies deletion retention. System 6 also
has a rendered pixel check for a white pasteboard without a soft document shadow. Historical
profiles render the default document shadow preference as a crisp outline using their border
token; an explicit None border is still honored.

Use a fresh caller-owned evidence directory; scale is `1`, `1.5`, or `2`, and renderer is
`cpu` or `gpu`. Both use WGPU for the offscreen UI; `gpu` additionally enables the production
GPU document canvas and fails if that path is unavailable.

```sh
cargo build --release -p photocraft-ui-egui --example runtime_theme_acceptance
PHOTOCRAFT_THEME_FILE="$PWD/plan/evidence/live-acceptance/live.theme.json" \
  target/release/examples/runtime_theme_acceptance "$PWD/plan/evidence/live-acceptance" 1 cpu
```

The example writes normal and recovered PNGs plus `acceptance.json` with the build mode and
asserted outcomes. This proves the native override path.

The desktop `theme_library_acceptance` example exercises the shared production native
library reader, atomic writer, background watcher, and Theme Manager in a release build.
It clicks Preview, Apply, Cancel, Reset Changes, Import, and Export; verifies persisted
selection and a fresh session; rejects malformed/oversized imports; and checks that
cancelled picks leave the draft and files alone. A valid library recovery clears its
previous error only while that message is still current, preserving unrelated status.
It also checks that external edits preserve drafts, defer during preview, and cannot
overwrite a newer Apply with a stale background read.

```sh
cargo build --release -p photocraft --example theme_library_acceptance
target/release/examples/theme_library_acceptance "$PWD/plan/evidence/library-acceptance" 1
```

Use a fresh evidence directory and leave `PHOTOCRAFT_THEME_FILE` unset; repeat at scales
`1.5` and `2` in separate directories. The example uses caller-owned paths injected at
the chooser boundary and the native bounded reader/atomic writer. It does not open or
visually verify the host file dialog, and does not access user settings or editing windows.
Browser import/export/persistence still requires separate runtime acceptance.

Historical Theme Manager windows use the theme's caption and close control. Dragging
the caption moves the window; closing it restores a preview like Cancel. Import and
Export follow the same theme button metrics and disabled treatment as the other actions.

Dock widths are editable with `dockWidth`, `dockMinWidth`, and `dockMaxWidth` (180–1024 logical points, ordered minimum ≤ default ≤ maximum). Historical presets use 240 by default and allow 220–520. Rendering and `ui.set {dockWidth}` share the resolved range. Fresh docks follow theme defaults; saved or explicitly resized widths remain user-owned and are clamped only when outside the new range.

### Optional historical font recreation

Native builds can load Chicago Kare from an external craft-fonts checkout at startup:
`PHOTOCRAFT_THEME_FONTS_DIR=/absolute/path/to/craft-fonts`. The loader reads only
`fonts/chicago-kare/ChicagoKare-Regular.ttf`, bounds reads to 8 MiB and verifies the actual
family name. It also recognizes that filename in the user's installed-font directory.
This runtime option does not embed the other craft-fonts assets or require recompilation.
Restart to load a newly added font; theme definitions and metrics remain live-editable.

`typography.family` accepts `Chicago Kare` explicitly. A `Chicago` request uses it when
original Chicago is absent, and Theme Manager reports the actual recreation name.
Chicago Kare is Duane King's MIT-licensed recreation, not Apple's original font.
Font bytes and their license/manifest/attribution live in craft-fonts, never PhotoCraft.
Browser builds still report their bundled fallback until a separate runtime font delivery
path is implemented; this native provider does not add font bytes to WebAssembly.
