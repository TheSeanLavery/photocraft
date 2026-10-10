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
- App-drawn geometry: `buttonHeight`, `fieldHeight`, `tabHeight`, `titleBarHeight`,
  `optionsBarHeight`, `statusBarHeight`, `toolButtonSize`, `railButtonSize` accept 16–96 points.
  `panelPadding`, `panelGap`, `rowGap`, `toolbarPadding`, `menuMargin`, `dialogMargin` accept
  0–32 points; `preferenceSpacing` is a horizontal/vertical pair, each 0–32 points.
  `buttonPadding` also controls horizontal padding in custom buttons. Defaults preserve existing
  themes. Buttons, numeric fields and tabs grow when needed to fit the configured type size.
- `typography.family`: optional registered UI family: `Inter`, `JetBrains Mono`,
  `Microsoft Sans Serif`, `Geneva`, `Chicago`, or `Charcoal`. Native builds read these faces
  from known installed-font locations, with bounded reads and parsing; no font files enter
  this repository. The built-in Inter stack remains the fallback. Missing Chicago/Charcoal
  use installed Geneva when available, then Inter. The manager reports substitutions;
  `ui.inspect.themeFont` exposes requested and actual family. Browser builds use the registered
  fallback stack when historical faces are unavailable. Current family registrations are cached
  for the app session; restart after installing an additional face.
- `typography`: egui text-style sizes, 6–64 points; `button`, `monospace` and `body` also size
  shared custom button text, numeric fields and pill tabs. Current named font families and other
  explicitly sized custom widgets retain their existing fonts and sizes.

Files are limited to 64 KiB. Numbers must be finite and within the documented ranges. The
loader keeps working across ordinary editor saves, including temporary invalid JSON and
atomic file replacement. It checks the active file every two seconds.

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
theme as an explicit live override outside a manager preview. Browser runtime verification,
historical widget styles and font selection are subsequent parts of the theme-engine work.


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
