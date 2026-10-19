"""Generate native icons from the shared SVG using resvg 0.45.1 and Pillow."""
import argparse
from pathlib import Path
import subprocess
import tempfile

from PIL import Image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resvg", default="resvg", help="path to resvg 0.45.1")
    args = parser.parse_args()
    version = subprocess.run(
        [args.resvg, "--version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if version != "0.45.1":
        parser.error(f"resvg 0.45.1 is required; found {version!r}")
    icons = Path(__file__).resolve().parent
    source = icons.parents[1] / "web" / "public" / "logo.svg"
    with tempfile.TemporaryDirectory(prefix="rsiversi-icons-") as temporary:
        rendered = Path(temporary) / "logo.png"
        subprocess.run(
            [args.resvg, "--width", "1024", str(source), str(rendered)], check=True
        )
        with Image.open(rendered) as image:
            if image.size != (1024, 1024):
                raise ValueError("The shared logo must have a square viewBox")
            canvas = image.convert("RGBA").resize((256, 256), Image.Resampling.LANCZOS)
    canvas.save(icons / "icon.png")
    sizes = [(size, size) for size in (16, 24, 32, 48, 64, 128, 256)]
    canvas.save(icons / "icon.ico", sizes=sizes)
    with Image.open(icons / "icon.png") as png, Image.open(icons / "icon.ico") as ico:
        actual_sizes = sorted(ico.ico.sizes())
        if png.size != (256, 256) or png.mode != "RGBA" or actual_sizes != sizes:
            raise ValueError("Generated icon dimensions or alpha channel differ from the contract")
        print({"png": png.size, "ico_sizes": actual_sizes})


if __name__ == "__main__":
    main()
