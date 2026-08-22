#!/usr/bin/env python3
"""Generate the full desktop icon set from public/logo-icon-taskbar.svg.

Outputs PNGs + ICO + ICNS into src-tauri/icons/.
Requires: ImageMagick (`convert`) and Pillow.
"""

import os
import subprocess
import tempfile
from pathlib import Path

from PIL import Image

try:
    from icnsutil import IcnsFile
except ImportError:
    IcnsFile = None

ROOT = Path(__file__).parent.parent
SVG_SRC = ROOT / "public" / "logo-icon-taskbar.svg"
OUT_DIR = ROOT / "src-tauri" / "icons"

# Base PNG sizes (named {w}x{h}.png)
BASE_SIZES = [16, 20, 24, 30, 32, 40, 44, 48, 64, 71, 89, 107, 128, 142, 150, 256, 284, 310]

# Windows tile sizes
TILE_SIZES = [30, 44, 71, 89, 107, 142, 150, 284, 310]

# Special outputs: (filename, size)
SPECIAL_SIZES = [
    ("icon.png", 512),          # main app icon
    ("StoreLogo.png", 256),     # Microsoft Store logo
    ("128x128@2x.png", 256),    # macOS retina
]

# ICNS supported sizes
ICNS_SIZES = [16, 32, 64, 128, 256, 512, 1024]

# ICO sizes
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def svg_to_png(svg_path: Path, size: int, out_path: Path) -> None:
    """Render an SVG to a PNG of the given size using ImageMagick."""
    subprocess.run(
        [
            "convert",
            "-background",
            "none",
            "-density",
            "300",
            "-resize",
            f"{size}x{size}",
            str(svg_path),
            str(out_path),
        ],
        check=True,
    )


def generate_pngs() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for size in BASE_SIZES:
        if size in TILE_SIZES:
            name = f"Square{size}x{size}Logo.png"
        else:
            name = f"{size}x{size}.png"
        svg_to_png(SVG_SRC, size, OUT_DIR / name)

    for name, size in SPECIAL_SIZES:
        svg_to_png(SVG_SRC, size, OUT_DIR / name)


def generate_ico() -> None:
    """Generate a multi-resolution ICO using ImageMagick."""
    with tempfile.TemporaryDirectory() as tmp:
        png_paths = []
        for size in ICO_SIZES:
            png_path = Path(tmp) / f"{size}.png"
            svg_to_png(SVG_SRC, size, png_path)
            png_paths.append(str(png_path))
        subprocess.run(
            [
                "convert",
                *png_paths,
                "-define",
                f"icon:auto-resize={','.join(map(str, ICO_SIZES))}",
                str(OUT_DIR / "icon.ico"),
            ],
            check=True,
        )


def generate_icns() -> None:
    """Generate a multi-resolution ICNS using icnsutil."""
    if IcnsFile is None:
        raise RuntimeError(
            "icnsutil is required to generate .icns files. "
            "Install it with: pip install icnsutil"
        )

    # ICNS type keys for standard square icon sizes.
    icns_keys = {
        16: "icp4",
        32: "icp5",
        64: "icp6",
        128: "ic07",
        256: "ic08",
        512: "ic09",
        1024: "ic10",
    }
    icns = IcnsFile()
    with tempfile.TemporaryDirectory() as tmp:
        for size in ICNS_SIZES:
            png_path = Path(tmp) / f"{size}.png"
            svg_to_png(SVG_SRC, size, png_path)
            icns.add_media(key=icns_keys[size], file=str(png_path))
    icns.write(OUT_DIR / "icon.icns")


def main() -> None:
    if not SVG_SRC.exists():
        raise SystemExit(f"Source SVG not found: {SVG_SRC}")

    generate_pngs()
    generate_ico()
    generate_icns()

    print(f"Icons generated in {OUT_DIR}")
    for f in sorted(OUT_DIR.iterdir()):
        print(f"  {f.name}")


if __name__ == "__main__":
    main()
