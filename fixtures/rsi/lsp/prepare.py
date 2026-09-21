"""Isolated, fixed-version rust-analyzer project and explicit Host leaves."""
import argparse, json, subprocess
from pathlib import Path
VERSION='rust-analyzer 1.97.0 (2d8144b 2026-07-07)'
SOURCE='pub trait Greet { fn greet(&self); }\npub struct Bird;\nimpl Greet for Bird { fn greet(&self) {} }\nfn main() { let _emoji = "😀"; let _bird = Bird; }\n'
SOURCE += ''.join(f'fn usage_{i}() {{ let _ = Bird; }}\n' for i in range(32))

def prepare(workspace, host, analyzer):
    workspace=Path(workspace).resolve();analyzer=Path(analyzer).absolute()
    assert subprocess.check_output([str(analyzer),'--version'],text=True).strip()==VERSION
    (workspace/'src').mkdir(exist_ok=True)
    (workspace/'src/main.rs').write_text(SOURCE)
    (workspace/'Cargo.toml').write_text('[package]\nname="language-fixture"\nversion="0.0.0"\nedition="2021"\n[workspace]\n')
    (workspace/'Cargo.lock').write_text('version=4\n[[package]]\nname="language-fixture"\nversion="0.0.0"\n')
    home=workspace/'.fixture-home';home.mkdir(exist_ok=True);(home/'cargo').mkdir(exist_ok=True)
    options={'cargo':{'buildScripts':{'enable':False},'sysroot':None},'procMacro':{'enable':False},'checkOnSave':False}
    config={'program':str(analyzer),'arguments':[],'environment':{'PATH':str(analyzer.parent)+':/usr/bin:/bin','HOME':str(home),'CARGO_HOME':str(home/'cargo')},'languages':{'.rs':'rust'},'initialization_options':options,'configuration':{'rust-analyzer':options}}
    host=Path(host)
    text=host.read_text().replace('steps = []','').replace('steps=[]','')
    text+='\n[[steps]]\nkind="plugin"\nid="language"\nplugin="rsi.lsp"\nconfig_json='+json.dumps(json.dumps(config,ensure_ascii=False),ensure_ascii=False)+'\n[[steps]]\nkind="plugin"\nid="language-ui"\nplugin="rsi.lsp.ui"\n'
    host.write_text(text)
    (workspace/'language-config.json').write_text(json.dumps(config))
    return {'line':4,'column':SOURCE.splitlines()[3].index('Bird')+1,'definition':'src/main.rs:2:12','version':VERSION}
if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('workspace');parser.add_argument('host');parser.add_argument('analyzer');args=parser.parse_args();print(json.dumps(prepare(args.workspace,args.host,args.analyzer)))
