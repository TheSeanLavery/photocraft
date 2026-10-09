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
    "controlSize": [18, 18]
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
- `typography`: standard egui text-style sizes, 6–64 points. Current named font families and
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
theme as an explicit live override. Theme Manager, browser file-picker import/download,
historical widget styles and font selection are subsequent parts of the theme-engine work.
