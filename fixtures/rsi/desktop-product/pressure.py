"""Real native write saturation, with OS reads released by filesystem barriers."""
import json
from controls import file_has_bytes


def verify_writes(script, button, terminal_keys, until, workspace, report):
    # Four independent PTYs occupy the remote API client Data lane.
    # Generic API capacity does not authorize automatic input replay.
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
        window.fixtureWritePressure={done:0,responses:[],ready:false};
        const pressure=window.fixtureWritePressure,base=JSON.parse(window.fixtureMainRead),extra=[],followers=[];
        fixtureTransport.observers.push((kind,reply,request)=>{
            if(kind==='call'&&reply.method==='terminal'){const input=JSON.parse(reply.payload).request;if(input.type==='write'&&input.attachment===base.request.attachment)pressure.uiWrites=(pressure.uiWrites??0)+1}
        });
        const request=command=>fixtureTransport.call('terminal',JSON.stringify({...base,request:command}));
        window.fixtureStartWrites=()=>{pressure.startedAt=performance.now();const bytes=Array(65536).fill(65);window.fixtureWriters=Promise.all(followers.map(attachment=>request({type:'write',attachment,bytes}).then(value=>{pressure.done++;pressure.responses.push(value.type)}))).catch(error=>{pressure.error=String(error);pressure.errorAfterMs=performance.now()-pressure.startedAt})};
        window.fixtureCloseWriters=async()=>{await window.fixtureWriters;for(const terminal of extra)await request({type:'close',terminal});pressure.cleaned=true};
        (async()=>{
            for(let i=0;i<4;i++){
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
    until(lambda: all((workspace / f'writer-ready-{index}').exists() for index in range(4)))
    script('window.fixtureStartWrites();return true')
    terminal_keys("printf 'native-busy-once' >> native-busy-result.txt")
    until(lambda: script('return document.querySelector(".terminal-notice")?.textContent.includes("Input was not confirmed")'))
    before_release = state()
    assert before_release['done'] == 0 and before_release['uiWrites'] > 0, before_release
    assert not (workspace / 'native-busy-result.txt').exists()
    (workspace / 'writer-release').touch()
    until(lambda: state()['done'] == 4)
    assert not (workspace / 'native-busy-result.txt').exists(), 'uncertain UI input was replayed'
    assert state()['uiWrites'] == before_release['uiWrites'], 'input retried without takeover'
    button('Take control')
    terminal_keys("printf 'native-busy-once' >> native-busy-result.txt")
    until(lambda: file_has_bytes(workspace / 'native-busy-result.txt', b'native-busy-once'))
    assert (workspace / 'native-busy-result.txt').read_bytes() == b'native-busy-once'
    for index in range(4):
        until(lambda: file_has_bytes(workspace / f'writer-bytes-{index}', b'A' * 65536))
        assert (workspace / f'writer-bytes-{index}').read_bytes() == b'A' * 65536
    script('void window.fixtureCloseWriters().catch(error=>window.fixtureWritePressure.error=String(error));return true')
    until(lambda: state().get('cleaned'))
    assert (workspace / 'native-busy-result.txt').read_bytes() == b'native-busy-once'
    (report / 'native-write-pressure.json').write_text(json.dumps({
        'beforeRelease': before_release, 'settled': state(), 'osReaders': 4,
        'writes': 4, 'uncertainInputNotReplayed': True, 'explicitTakeover': True, 'bytesPerReader': 65536, 'exactUIBytes': 'native-busy-once',
        'boundary': 'real API Data pressure, failed-closed document input and OS PTY; no mocked responses',
    }, indent=2))
