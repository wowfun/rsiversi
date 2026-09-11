"""Real production Tauri/WebKitGTK document input/paint and process-family PSS."""
import argparse
import base64
import http.client
import json
import math
import os
from pathlib import Path
import socket
import statistics
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
for name in ('binary', 'driver', 'documents', 'report'):
    parser.add_argument('--' + name, type=Path, required=True)
parser.add_argument('--smoke', action='store_true')
args = parser.parse_args(); args.report.mkdir(parents=True, exist_ok=False)
def until(read, timeout=30):
    end = time.monotonic() + timeout
    while True:
        result = read()
        if result: return result
        if time.monotonic() >= end: raise TimeoutError('performance fixture condition')
        time.sleep(.025)
def family_pss(root):
    parents = {}
    for proc in Path('/proc').iterdir():
        if not proc.name.isdecimal(): continue
        try:
            stat = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
            parents[int(proc.name)] = int(stat[1])
        except (OSError, ValueError): pass
    selected = {root}
    while True:
        children = {pid for pid, parent in parents.items() if parent in selected}
        if children <= selected: break
        selected |= children
    result = []
    for pid in sorted(selected - {root}):
        try:
            proc = Path('/proc') / str(pid)
            pss = next(int(line.split()[1]) for line in (proc / 'smaps_rollup').read_text().splitlines() if line.startswith('Pss:'))
            result.append({'pid': pid, 'name': (proc / 'comm').read_text().strip(), 'pss_kib': pss})
        except (OSError, StopIteration): pass
    if not any('WebKitWeb' in item['name'] for item in result): raise RuntimeError('WebKit web process absent from PSS family')
    if not any('WebKitNetwork' in item['name'] for item in result): raise RuntimeError('WebKit network process absent from PSS family')
    return result
reports = []
for variant in ('baseline', 'current'):
    output = args.report / variant; output.mkdir()
    env = {key: value for key, value in os.environ.items() if key in ('PATH','DISPLAY','DBUS_SESSION_BUS_ADDRESS','LANG','XAUTHORITY')}
    env['TAURI_WEBVIEW_AUTOMATION'] = 'true'
    for key in ('HOME','XDG_CONFIG_HOME','XDG_STATE_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','XDG_RUNTIME_DIR'):
        path = output / key.lower(); path.mkdir(mode=0o700); env[key] = str(path.resolve())
    config = Path(env['XDG_CONFIG_HOME']) / 'rsi'; host = config / 'host-profiles/fixture'; host.mkdir(parents=True)
    (host/'host.profile.toml').write_text('format = 1\nsteps = []\n'); (config/'settings.json').write_text('{"rsi.agent":{}}')
    with socket.socket() as sock: sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]
    log = (output/'webdriver.log').open('w')
    driver = subprocess.Popen([str(args.driver.resolve()),'--host=127.0.0.1',f'--port={port}'],env=env,stdout=log,stderr=subprocess.STDOUT)
    session = None
    def call(method,path,body=None):
        connection = http.client.HTTPConnection('127.0.0.1',port,timeout=35)
        try:
            connection.request(method,path,None if body is None else json.dumps(body),{'Content-Type':'application/json'})
            response=connection.getresponse(); result=json.loads(response.read(32*1024*1024))
            if response.status >= 400: raise RuntimeError(result)
            return result.get('value')
        finally: connection.close()
    try:
        def ready():
            try: return call('GET','/status')
            except (OSError,http.client.HTTPException): return None
        until(ready)
        capabilities=call('POST','/session',{'capabilities':{'alwaysMatch':{'webkitgtk:browserOptions':{'binary':str(args.binary.resolve()),'args':['--assets',str((args.documents/variant).resolve()),'--host-profile','fixture']}}}})
        session=capabilities['sessionId']; root=f'/session/{session}'
        def script(source,arguments=None): return call('POST',root+'/execute/sync',{'script':source,'args':arguments or []})
        until(lambda:script('return !!window.rsiPerformance'),45)
        measurements=[]
        for count in ([16] if args.smoke else [16,64,128]):
            for run in range(1 if args.smoke else 10):
                script('window.perfReady=null;window.rsiPerformance.setup(...arguments).then(value=>window.perfReady=value).catch(e=>window.perfReady={error:String(e)});return true',[count,run])
                setup=until(lambda:script('return window.perfReady'))
                assert setup.get('blocks') == count and setup.get('input',0) >= 180 and 'Result 0:' in setup.get('body',''), setup
                element=script('return document.querySelector("textarea[aria-label$=message]" )')
                identity=next(iter(element.values()))
                for _ in range(5):
                    before=script('return window.rsiPerformance.samples.length')
                    call('POST',root+f'/element/{identity}/value',{'text':'x','value':['x']})
                    until(lambda:script('return window.rsiPerformance.samples.length')>before)
                assert script('return document.querySelector("textarea[aria-label$=message]").value') == 'xxxxx'
                assert script('return [...document.querySelectorAll(".message-text")].at(-1).textContent') == 'Streaming update xxxxx'
                pss=[]
                # Settling samples are retained, rather than silently choosing a low point.
                for _ in range(3):
                    time.sleep(.2); family=family_pss(driver.pid)
                    pss.append({'processes':family,'total_kib':sum(item['pss_kib'] for item in family)})
                measurements.append({'blocks':count,'run':run,'pss':pss})
                (output/'progress.json').write_text(json.dumps({'measurements':measurements,'samples':script('return window.rsiPerformance.samples')},indent=2))
            (output/f'{count}-blocks.png').write_bytes(base64.b64decode(call('GET',root+'/screenshot')))
        trajectory = None
        if not args.smoke:
            script('window.perfTrajectory=null;void window.rsiPerformance.trajectory().then(value=>window.perfTrajectory=value);return true')
            trajectory=until(lambda:script('return window.perfTrajectory'))
            assert trajectory['laidOut']==128 and trajectory['tools']>=40 and trajectory['reasoning']>=40, trajectory
            (output/'trajectory-128.png').write_bytes(base64.b64decode(call('GET',root+'/screenshot')))
        samples=script('return window.rsiPerformance.samples')
        report={'variant':variant,'capabilities':capabilities['capabilities'],'samples':samples,'measurements':measurements,'trajectory':trajectory}
        (output/'result.json').write_text(json.dumps(report,indent=2)); reports.append(report)
        script('void window.rsiPerformance.close();return true')
        until(lambda:'desktop: main-thread exit after Application cleanup' in (output/'webdriver.log').read_text(),45)
    except Exception:
        if session:
            try:
                (output/'failure.html').write_text(script('return document.documentElement.outerHTML'))
                (output/'failure.png').write_bytes(base64.b64decode(call('GET',root+'/screenshot')))
            except Exception: pass
        raise
    finally:
        if session:
            try: call('DELETE',f'/session/{session}')
            except (OSError,RuntimeError,http.client.HTTPException): pass
        driver.terminate()
        try: driver.wait(timeout=10)
        except subprocess.TimeoutExpired: driver.kill(); driver.wait()
        log.close()
summary=[]
for count in ([16] if args.smoke else [16,64,128]):
    row={'blocks':count}
    for report in reports:
        values=sorted(item['input_to_paint_ms'] for item in report['samples'] if item['blocks']==count)
        pss=[statistics.median(x['total_kib'] for x in item['pss']) for item in report['measurements'] if item['blocks']==count]
        row[report['variant']]={'input_to_paint_p95_ms':values[math.ceil(len(values)*.95)-1],'steady_pss_kib':statistics.median(pss),'samples':len(values)}
    row['pss_ratio']=row['current']['steady_pss_kib']/row['baseline']['steady_pss_kib']; summary.append(row)
(args.report/'summary.json').write_text(json.dumps({'smoke':args.smoke,'platform':{key:getattr(os.uname(),key) for key in ('sysname','nodename','release','version','machine')},'scenes':summary},indent=2))
print(json.dumps(summary))
