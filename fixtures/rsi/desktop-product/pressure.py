"""Real native write saturation, with OS reads released by filesystem barriers."""
import json


def verify_writes(script, terminal_keys, until, workspace, report):
    # Each raw PTY stops consuming input until all eight writers are pending.
    # No response or request dispatch is mocked.
    (workspace / 'pressure-reader.py').write_text('''import os,sys,time
from pathlib import Path
index=sys.argv[1]
Path('writer-ready-'+index).touch()
while not Path('writer-release').exists(): time.sleep(.01)
data=bytearray()
while len(data)<65536:
    chunk=os.read(0,65536-len(data))
    if not chunk: raise RuntimeError('incomplete pressure input')
    data.extend(chunk)
Path('writer-bytes-'+index).write_bytes(data)
''')
    script(r'''
        window.fixtureWritePressure={busy:0,done:0,responses:[],ready:false};
        const pressure=window.fixtureWritePressure,base=JSON.parse(window.fixtureMainRead),extra=[],followers=[base.request.attachment];
        const original=window.fetch;
        window.fetch=(path,options)=>{
            const response=original(path,options);
            if(String(path)==='/_call/terminal'&&JSON.parse(options.body).request?.type==='write')
                void response.then(reply=>reply.clone().json()).then(value=>{if(value.code==='busy')pressure.busy++}).catch(()=>{});
            return response;
        };
        const request=async command=>{
            const response=await fetch('/_call/terminal',{method:'POST',body:JSON.stringify({...base,request:command})});
            const value=JSON.parse(await response.text());if(!response.ok)throw Error(JSON.stringify(value));return value;
        };
        window.fixtureStartWrites=()=>{const bytes=Array(65536).fill(65);window.fixtureWriters=Promise.all(followers.map(attachment=>request({type:'write',attachment,bytes}).then(value=>{pressure.done++;pressure.responses.push(value.type)}))).catch(error=>pressure.error=String(error))};
        window.fixtureCloseWriters=async()=>{await window.fixtureWriters;for(const terminal of extra)await request({type:'close',terminal});pressure.cleaned=true};
        (async()=>{
            for(let i=1;i<8;i++){
                const created=await request({type:'create',size:{rows:24,columns:80}});
                extra.push(created.value.terminal.id);followers.push(created.value.id);
                await request({type:'write',attachment:created.value.id,bytes:Array.from(new TextEncoder().encode('stty raw -echo; python3 pressure-reader.py '+i+'; stty sane\r'))});
            }
            pressure.ready=true;
        })().catch(error=>pressure.error=String(error));return true;
    ''')
    def state():
        value = script('return window.fixtureWritePressure')
        assert 'error' not in value, value
        return value
    until(lambda: state()['ready'])
    terminal_keys('stty raw -echo; python3 pressure-reader.py 0; stty sane')
    until(lambda: all((workspace / f'writer-ready-{index}').exists() for index in range(8)))
    script('window.fixtureStartWrites();return true')
    # The UI input pump receives a real ninth native write rejection and keeps
    # this command until the already-admitted OS writes can finish.
    terminal_keys("printf 'native-busy-once' >> native-busy-result.txt")
    until(lambda: state()['busy'] > 0)
    before_release = state()
    assert before_release['done'] == 0, before_release
    (workspace / 'writer-release').touch()
    until(lambda: state()['done'] == 8)
    until(lambda: (workspace / 'native-busy-result.txt').exists())
    assert (workspace / 'native-busy-result.txt').read_bytes() == b'native-busy-once'
    for index in range(8):
        until(lambda: (workspace / f'writer-bytes-{index}').exists())
        assert (workspace / f'writer-bytes-{index}').read_bytes() == b'A' * 65536
    script('void window.fixtureCloseWriters().catch(error=>window.fixtureWritePressure.error=String(error));return true')
    until(lambda: state().get('cleaned'))
    assert (workspace / 'native-busy-result.txt').read_bytes() == b'native-busy-once'
    (report / 'native-write-pressure.json').write_text(json.dumps({
        'beforeRelease': before_release, 'settled': state(), 'osReaders': 8,
        'bytesPerReader': 65536, 'exactUIBytes': 'native-busy-once',
        'boundary': 'real native rejection, document input pump and OS PTY; no mocked responses',
    }, indent=2))
