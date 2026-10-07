# PhotoCraft app icon

The canonical logo is a vector recreation of the project owner's supplied orange filmstrip
mark: a rounded golden frame with four dark sprocket holes per side, an orange inset, and dark
“Pc” lettering. The master is geometric SVG artwork, with no embedded raster image or font.
The corners outside the tile are transparent. The dark colour appears only in the sprocket
holes and lettering.

The macOS render pads the rounded 512-unit tile onto the platform icon grid. Windows and
Linux render the tile tightly with a slightly smaller corner radius, so the mark fills small
shell icons without clipping the frame or sprocket holes.

## Files

- `photocraft.svg`: full-detail vector master on a 512-unit square.
- `photocraft-small.svg`: flat-colour version of the tighter icon for tiny/scalable icons.
- `photocraft-1024.png`, `photocraft.icns`, `photocraft.ico`, and `hicolor/`: generated
  macOS, Windows, Linux and runtime icons.

`apps/photocraft/src/app_icon.rs` loads the generated PNG at runtime. The platform packages
consume the `.icns`, `.ico`, and hicolor assets.

## Regenerate

Run `packaging/icons.sh` after changing the SVG. It uses `resvg` or `rsvg-convert`, `iconutil`
on macOS, and `cargo xtask ico` to pack the Windows icon.
