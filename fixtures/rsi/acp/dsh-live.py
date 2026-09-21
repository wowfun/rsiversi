"""Opt-in RSI Host to pinned DSH ACP Agent with real DeepSeek and private MCP."""
import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--runtime',type=Path,required=True)
parser.add_argument('--source',type=Path,required=True)
parser.add_argument('--env-file',type=Path,required=True)
parser.add_argument('--report',type=Path,required=True)
args=parser.parse_args()
assert subprocess.check_output(['git','-C',str(args.source),'rev-parse','HEAD'],text=True).strip()=='ddefc45fbc7f8e46dd73185e68295696d1297887'
source=args.env_file.read_bytes();assert len(source)<=65536
match=re.search(r'^\s*(?:export\s+)?DEEPSEEK_API_KEY\s*=\s*(.*?)\s*$',source.decode(),re.M);assert match
key=re.sub(r'''^(["'])(.*)\1$''',r'\2',match[1].strip());assert key
env=os.environ.copy();env.update(RSI_DSH_LIVE_KEY=key,RSI_DSH_REPORT=str(args.report.resolve()),RSI_DSH_RUNTIME=str(args.runtime.resolve()),RSI_DSH_NODE=shutil.which('node'))
result=subprocess.run(['cargo','test','--locked','-p','rsi-acp-host','--test','host','pinned_dsh_live','--','--ignored','--nocapture'],env=env,timeout=420)
raise SystemExit(result.returncode)
