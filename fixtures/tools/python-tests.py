#!/usr/bin/env python3
"""Run a consumer's unittest suite; a missing or empty suite is a failed gate."""
import argparse
from pathlib import Path
import sys
import unittest

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path)
args = parser.parse_args()
suite = unittest.defaultTestLoader.discover(str(args.directory.resolve()), pattern="test_*.py")
if not suite.countTestCases():
    parser.error("test discovery found zero cases")
sys.exit(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
