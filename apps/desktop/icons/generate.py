"""Render the committed rect/circle SVG, preserving one artwork source."""
from pathlib import Path
from xml.etree import ElementTree
from PIL import Image, ImageDraw

icons = Path(__file__).resolve().parent
scale = 4
canvas = Image.new("RGBA", (256 * scale, 256 * scale), (0, 0, 0, 0))
draw = ImageDraw.Draw(canvas)
for element in ElementTree.parse(icons / "icon.svg").getroot():
    attrs = element.attrib
    kind = element.tag.rsplit("}", 1)[-1]
    if kind == "rect":
        x, y, width, height = (float(attrs[k]) * scale for k in ("x", "y", "width", "height"))
        draw.rounded_rectangle((x, y, x + width - 1, y + height - 1), radius=float(attrs.get("rx", 0)) * scale, fill=attrs["fill"])
    elif kind == "circle":
        x, y, radius = (float(attrs[k]) * scale for k in ("cx", "cy", "r"))
        draw.ellipse((x - radius, y - radius, x + radius - 1, y + radius - 1), fill=attrs["fill"])
    else:
        raise ValueError(kind)
canvas = canvas.resize((256, 256), Image.Resampling.LANCZOS)
canvas.save(icons / "icon.png")
canvas.save(icons / "icon.ico", sizes=[(n, n) for n in (16, 24, 32, 48, 64, 128, 256)])
print({"png": canvas.size, "ico_sizes": sorted(Image.open(icons / "icon.ico").ico.sizes())})
