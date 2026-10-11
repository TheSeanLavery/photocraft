#!/usr/bin/env python3
"""Publish generated web assets to R2, uploading index.html last for atomic references."""
import argparse
import hashlib
import json
import mimetypes
import subprocess
from pathlib import Path


def main():
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dist", type=Path, default=root / "dist/web")
    parser.add_argument("--prefix", default="room-panel", help="R2 preview namespace (preserves existing editor assets)")
    args = parser.parse_args()
    if not args.prefix or not all(c.isascii() and (c.isalnum() or c == '-') for c in args.prefix):
        parser.error("prefix must contain ASCII letters, numbers or hyphens")
    files = sorted(p for p in args.dist.iterdir() if p.is_file())
    index = args.dist / "index.html"
    if len([p for p in files if p.suffix == ".wasm"]) != 1 or not index.is_file():
        parser.error("Build first: NO_COLOR=true trunk build --release --config apps/photocraft-web/Trunk.toml")
    print(json.dumps({p.name: {"bytes": p.stat().st_size, "sha256": hashlib.sha256(p.read_bytes()).hexdigest()} for p in files}, indent=2), flush=True)
    for path in [p for p in files if p.name != "index.html"] + [index]:
        mime = "application/wasm" if path.suffix == ".wasm" else mimetypes.guess_type(path.name)[0] or "application/octet-stream"
        subprocess.run(["wrangler", "r2", "object", "put", "photocraft-web-assets/" + args.prefix + "/" + path.name, "--file", str(path.resolve()), "--content-type", mime, "--remote"], check=True, cwd=Path(__file__).resolve().parent)
    print("Uploaded preview assets. Deploy the tested room Worker with wrangler deploy.")


if __name__ == "__main__":
    main()
