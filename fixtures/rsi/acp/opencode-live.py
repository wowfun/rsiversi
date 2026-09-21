"""Opt-in actual RSI browser/Host -> installed OpenCode ACP, with exact model/effort."""
import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--opencode', type=Path, default=Path(shutil.which('opencode') or 'opencode'))
parser.add_argument('--auth', type=Path, default=Path.home()/'.local/share/opencode/auth.json')
parser.add_argument('--assets', type=Path, required=True)
parser.add_argument('--report', type=Path, required=True)
parser.add_argument('--fallback-after', type=Path, help='Reuse an earlier recorded primary availability failure')
args = parser.parse_args()
args.report.mkdir(parents=True, exist_ok=False)
assert args.auth.stat().st_size <= 65536
auth = json.loads(args.auth.read_text())
auth = {key:value for key,value in auth.items() if key in {'opencode','opencode-go'}}
assert auth, 'OpenCode credentials for the requested providers are required'
secrets = [value for entry in auth.values() for key,value in entry.items()
           if key in {'key','access','refresh'} and isinstance(value,str) and value]
version = subprocess.check_output([str(args.opencode.resolve()),'--version'], text=True).strip()
attempts = []
candidates = [
    ('opencode/muse-spark-1.3-contributor-free', 'xhigh'),
    ('opencode-go/deepseek-v4.1-flash', 'max'),
]
if args.fallback_after:
    assert args.fallback_after.stat().st_size <= 1024*1024
    previous = json.loads(args.fallback_after.read_text())
    primary = next(item for item in previous if item.get('model') == candidates[0][0]
                   and item.get('effort') == 'xhigh' and item.get('phase') == 'availability'
                   and item.get('fallback_eligible') is True)
    assert any(item.get('error',{}).get('statusCode') in {401,402,403,404,429,503}
               for item in primary['actual_messages'])
    attempts.append({**primary, 'reused_evidence':str(args.fallback_after.resolve())})
    candidates = candidates[1:]

def messages(root):
    database = root/'data/opencode/opencode.db'
    if not database.exists():
        return []
    with sqlite3.connect(f'file:{database}?mode=ro', uri=True) as connection:
        return [json.loads(row[0]) for row in connection.execute('select data from message order by time_created, id')]

for index, (model, effort) in enumerate(candidates, start=len(attempts)):
    destination = args.report/f'attempt-{index+1}'
    with tempfile.TemporaryDirectory(prefix='rsi-opencode-live-') as directory:
        private = Path(directory)
        data = private/'data/opencode'
        data.mkdir(parents=True)
        auth_path = data/'auth.json'
        auth_path.write_text(json.dumps(auth))
        auth_path.chmod(0o600)
        config = {'model':model,'enabled_providers':['opencode','opencode-go'],
                  'autoupdate':False,'share':'disabled','plugin':[],
                  'permission':{'*':'deny','read':'allow','edit':'ask'}}
        child_environment = {
            'PATH':os.environ.get('PATH','/usr/bin:/bin'), 'HOME':str(Path.home()),
            'XDG_DATA_HOME':str(private/'data'), 'XDG_CONFIG_HOME':str(private/'config'),
            'XDG_STATE_HOME':str(private/'state'), 'XDG_CACHE_HOME':str(private/'cache'),
            'OPENCODE_CONFIG_DIR':str(private/'config/opencode'),
            'OPENCODE_CONFIG_CONTENT':json.dumps(config),
            'OPENCODE_DISABLE_AUTOUPDATE':'1','OPENCODE_DISABLE_CLAUDE_CODE':'1',
            'OPENCODE_DISABLE_PROJECT_CONFIG':'1',
        }
        for name in ['HTTP_PROXY','HTTPS_PROXY','ALL_PROXY','NO_PROXY','SSL_CERT_FILE','NODE_EXTRA_CA_CERTS']:
            if os.environ.get(name):
                child_environment[name] = os.environ[name]
        launch = private/'fixture.json'
        launch.write_text(json.dumps({'binary':str(args.opencode.resolve()),'model':model,
                                     'effort':effort,'environment':child_environment}))
        environment = os.environ.copy()
        environment.update(RSI_OPENCODE_FIXTURE=str(launch), RSI_WEB_ASSETS=str(args.assets.resolve()))
        with (args.report/f'attempt-{index+1}.log').open('w') as log:
            completed = subprocess.run(['node','fixtures/rsi/acp/opencode-browser.mjs',str(destination.resolve())],
                                       env=environment,stdout=log,stderr=subprocess.STDOUT,timeout=600)
        result_path = destination/'result.json'
        result = json.loads(result_path.read_text()) if result_path.exists() else {'ok':False,'phase':'infrastructure'}
        observed = messages(private)
        evidence = []
        unavailable = False
        for message in observed:
            error = message.get('error') or {}
            error_data = error.get('data') or {}
            code = error_data.get('statusCode')
            if error.get('name') == 'APIError' and code in {401,402,403,404,429,503}:
                unavailable = True
            evidence.append({key:message[key] for key in ['role','model','providerID','modelID','variant','finish'] if key in message})
            if error:
                evidence[-1]['error'] = {'name':error.get('name'),'statusCode':code}
        result.update(cli_version=version,actual_messages=evidence,exit=completed.returncode)
        if result.get('ok'):
            user_messages = [item for item in observed if item.get('role') == 'user']
            provider, identifier = model.split('/',1)
            confirmed = len(user_messages) == 3 and all(item.get('model',{}).get('providerID') == provider
                       and item.get('model',{}).get('modelID') == identifier
                       and item.get('model',{}).get('variant') == effort
                       for item in user_messages)
            result['actual_model_and_effort_verified'] = confirmed
            if not confirmed:
                result.update(ok=False, error='Actual OpenCode request selection differs')
        result['fallback_eligible'] = not result.get('ok') and result.get('phase') == 'availability' and unavailable
        attempts.append(result)
        (args.report/'result.json').write_text(json.dumps(attempts,indent=2)+'\n')
        for path in args.report.rglob('*'):
            if path.is_file() and path.suffix not in {'.png'} and path.stat().st_size <= 32*1024*1024:
                content = path.read_bytes()
                assert not any(secret.encode() in content for secret in secrets), 'credential in retained evidence'
        print(json.dumps({key:result[key] for key in ['model','effort','ok','phase','exit','fallback_eligible'] if key in result}),flush=True)
        if result.get('ok') or not result['fallback_eligible']:
            break
assert attempts[-1].get('ok'), 'OpenCode ACP acceptance did not pass; see sanitized report'
