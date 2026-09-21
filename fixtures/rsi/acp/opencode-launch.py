"""Record the managed ACP process identity, then replace this process with OpenCode."""
import os
from pathlib import Path
import sys

marker, binary, workspace = sys.argv[1:]
with Path(marker).open('a', encoding='utf-8') as stream:
    stream.write(str(os.getpid()) + '\n')
os.execv(binary, [binary, 'acp', '--pure', '--cwd', workspace])
