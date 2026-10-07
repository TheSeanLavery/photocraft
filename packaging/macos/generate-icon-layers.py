#!/usr/bin/env python3
"""Extract unmasked Icon Composer SVG layers from the canonical PhotoCraft SVG."""
from pathlib import Path
import copy
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
MASTER = ROOT / "assets/app-icon/photocraft.svg"
ASSETS = ROOT / "assets/app-icon/PhotoCraft.icon/Assets"
NS = "http://www.w3.org/2000/svg"
ET.register_namespace("", NS)


def layer(name: str, shapes: list[ET.Element], fill: str) -> None:
    root = ET.Element(f"{{{NS}}}svg", {"viewBox": "0 0 512 512"})
    group = ET.SubElement(root, f"{{{NS}}}g", {"fill": fill})
    for shape in shapes:
        item = copy.deepcopy(shape)
        item.attrib.pop("fill", None)
        group.append(item)
    ET.indent(root, space="  ")
    with (ASSETS / name).open("w", encoding="utf-8") as output:
        output.write('<?xml version="1.0" encoding="UTF-8"?>\n')
        output.write(ET.tostring(root, encoding="unicode"))
        output.write("\n")


def main() -> None:
    source = ET.parse(MASTER).getroot()
    panel = source.find(f"{{{NS}}}rect[@x='83']")
    ink = source.find(f"{{{NS}}}g")
    if panel is None or ink is None:
        raise SystemExit("error: canonical icon is missing its panel or ink group")
    holes = ink.findall(f"{{{NS}}}rect")
    letters = ink.findall(f"{{{NS}}}path")
    if len(holes) != 8 or len(letters) != 2:
        raise SystemExit("error: expected eight perforations and two letter paths")
    ASSETS.mkdir(parents=True, exist_ok=True)
    layer("01-panel.svg", [panel], "#f36500")
    layer("02-perforations.svg", holes, "#2b0e02")
    layer("03-lettering.svg", letters, "#2b0e02")


if __name__ == "__main__":
    main()
