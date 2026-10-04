import {execFileSync} from 'node:child_process';
// Keyless opt-in Node setup shared by deterministic and explicit live fixtures.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {join,isAbsolute} from 'node:path';
export async function configureProgram({config,node=process.env.RSI_TEST_NODE}) {
  assert(node && isAbsolute(node),'explicit absolute RSI_TEST_NODE required');
  const profile = join(config,'host-profiles/fixture/host.profile.toml');
  const source = (await readFile(profile,'utf8')).replace(/^steps\s*=\s*\[\]\s*$/m,'');
  await writeFile(profile, source + `\n[[steps]]\nkind="patch"\ntarget="program-runtime"\nconfig_json=${JSON.stringify(JSON.stringify({node}))}\n[[steps]]\nkind="patch"\ntarget="program-runtime"\nenabled=true\n`);
  const settings=JSON.parse(await readFile(join(config,'settings.json'),'utf8'));
  settings['rsi.agent-presets']={default:'workflow'};
  await writeFile(join(config,'settings.json'),JSON.stringify(settings));
}

// Sandbox user namespaces may deny /proc/<pid>/cwd or /exe. Match the owning
// Host's actual descendant chain and (when supplied) its reported namespace PID.
export async function nativePrograms(service, namespacePid, diagnostic=false) {
  const {execFileSync}=await import('node:child_process');
  const script=`import json,pathlib,sys
owner=int(sys.argv[1]); ns=int(sys.argv[2]) if sys.argv[2] else None
rows={}; failures=[]
for p in pathlib.Path('/proc').iterdir():
 if not p.name.isdigit(): continue
 try:
  status=dict(line.split(':',1) for line in (p/'status').read_text().splitlines() if ':' in line)
  row={'pid':int(p.name),'parent':int(status['PPid'].strip()),'name':status['Name'].strip(),'namespace_pids':[int(v) for v in status.get('NSpid',str(p.name)).split()]}
  rows[int(p.name)]=row
  row['start_time']=(p/'stat').read_text().rsplit(')',1)[1].split()[19]
 except (OSError,ValueError,PermissionError) as error: failures.append({'pid':int(p.name),'error':type(error).__name__})
found=[]
descendants=[]
for row in rows.values():
 parent=row['parent']; seen=set()
 while parent and parent not in seen:
  if parent==owner:
   descendants.append(row)
   if row['name'] in ('node','MainThread','node-MainThread') and (ns is None or row['namespace_pids'][-1]==ns): found.append(row)
   break
  seen.add(parent); parent=rows.get(parent,{}).get('parent',0)
print(json.dumps({"owner":owner,"selected":found,"descendants":descendants,"failures":failures}))`;
  const result=JSON.parse(execFileSync('python3',['-c',script,String(service.pid),namespacePid===undefined?'':String(namespacePid)],{encoding:'utf8'}));
  return diagnostic?result:result.selected;
}

// Inspect the fixture-owned Store without modifying canonical evidence.
export function programEvidence(workspace) {
  const script=`import json,sqlite3,pathlib,sys
paths=list((pathlib.Path(sys.argv[1]).parent/'state').rglob('sessions.sqlite3'))
assert len(paths)==1,paths
db=sqlite3.connect('file:'+str(paths[0])+'?mode=ro',uri=True)
print(json.dumps({kind:[{'session_id':s,'record':json.loads(v)} for s,v in db.execute(sql)] for kind,sql in [('controls','SELECT session_id,control_json FROM agent_controls ORDER BY session_id,seq'),('facts','SELECT session_id,fact_json FROM facts ORDER BY session_id,seq')]}))`;
  return JSON.parse(execFileSync('python3',['-c',script,workspace],{encoding:'utf8',maxBuffer:16*1024*1024}));
}

// Matches the Session UI's actual cancelling feedback, including its casing.
export function assertNoPendingWorkflowCleanup(text) {
  assert.doesNotMatch(text, /cleanup pending/i,
    'terminal readback must clear pending cleanup feedback');
}
