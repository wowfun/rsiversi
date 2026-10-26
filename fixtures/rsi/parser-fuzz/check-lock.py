"""Reject shared fuzz dependencies resolved outside the product lockfile."""
from pathlib import Path
import sys
import tomllib

root = Path(__file__).resolve().parents[3]
product = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))["package"]
fixture = tomllib.loads(Path(__file__).with_name("Cargo.lock").read_text(encoding="utf-8"))["package"]
versions = {}
for package in product:
    versions.setdefault(package["name"], set()).add((package["version"], package.get("source")))
drift = [f'{package["name"]} {package["version"]}' for package in fixture
         if package["name"] in versions and (package["version"], package.get("source")) not in versions[package["name"]]]
if drift:
    sys.exit("Fuzz/product dependency drift:\n" + "\n".join(sorted(drift)))
print("Fuzz dependencies match product resolutions; harness-only dependencies excluded.")
