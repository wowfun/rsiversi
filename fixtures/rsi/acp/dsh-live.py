"""Opt-in RSI Host to pinned DSH ACP Agent with real DeepSeek and private MCP."""
import argparse
import os
from pathlib import Path
import sys
import shutil
import subprocess

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from live import read_deepseek_key

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--runtime',type=Path,required=True)
parser.add_argument('--source',type=Path,required=True)
parser.add_argument('--env-file',type=Path,required=True)
parser.add_argument('--report',type=Path,required=True)
args=parser.parse_args()
assert subprocess.check_output(['git','-C',str(args.source),'rev-parse','HEAD'],text=True).strip()=='ddefc45fbc7f8e46dd73185e68295696d1297887'
key=read_deepseek_key(args.env_file)
env=os.environ.copy();env.update(RSI_DSH_LIVE_KEY=key,RSI_DSH_REPORT=str(args.report.resolve()),RSI_DSH_RUNTIME=str(args.runtime.resolve()),RSI_DSH_NODE=shutil.which('node'))
result=subprocess.run(['cargo','test','--locked','-p','rsi-acp-host','--test','host','pinned_dsh_live','--','--ignored','--nocapture'],env=env,timeout=420)
raise SystemExit(result.returncode)
