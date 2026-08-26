"""Rebuild assets/deskfence.ico from the vector source assets/deskfence-icon.svg.

Usage:
    python tools/rebuild_icon.py            # rebuild .ico only
    python tools/rebuild_icon.py --pngs     # also dump per-size PNGs next to the .ico

Requires: pip install resvg-py
After running, rebuild the exe (cargo build --release) so the embedded icon updates.
"""
import pathlib
import struct
import sys

import resvg_py

ROOT = pathlib.Path(__file__).resolve().parents[1]
SVG = ROOT / "assets" / "deskfence-icon.svg"
ICO = ROOT / "assets" / "deskfence.ico"
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def main() -> None:
    svg = SVG.read_text(encoding="utf-8")
    pngs: dict[int, bytes] = {}
    for size in SIZES:
        data = resvg_py.svg_to_bytes(svg_string=svg, width=size, height=size)
        pngs[size] = data[0] if isinstance(data, list) else data

    ico = bytearray(struct.pack("<HHH", 0, 1, len(SIZES)))
    offset = 6 + 16 * len(SIZES)
    for size in SIZES:
        dim = 0 if size >= 256 else size
        ico += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(pngs[size]), offset)
        offset += len(pngs[size])
    for size in SIZES:
        ico += pngs[size]
    ICO.write_bytes(bytes(ico))
    print(f"wrote {ICO} ({len(ico)} bytes, {len(SIZES)} sizes)")

    if "--pngs" in sys.argv:
        for size, data in pngs.items():
            p = ICO.with_name(f"deskfence-{size}.png")
            p.write_bytes(data)
            print(f"wrote {p}")


if __name__ == "__main__":
    main()
