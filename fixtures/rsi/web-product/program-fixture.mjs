import {execFileSync} from 'node:child_process';
import {DatabaseSync} from 'node:sqlite';
import {storedFact} from './fact-evidence.mjs';
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
  const result=JSON.parse(execFileSync('python3',['-c',script,String(service.foregroundPids[0]),namespacePid===undefined?'':String(namespacePid)],{encoding:'utf8'}));
  return diagnostic?result:result.selected;
}

// Inspect the fixture-owned Store without modifying canonical evidence.
export function programEvidence(workspace) {
  const db=new DatabaseSync(join(workspace,'../state/rsiversi/agent/sessions.sqlite3'),{readOnly:true});
  try {
    db.exec('BEGIN');
    const {bytes}=db.prepare(`SELECT coalesce((SELECT sum(coalesce(logical_bytes,length(CAST(fact_json AS BLOB)))) FROM facts),0)
      + coalesce((SELECT sum(length(CAST(control_json AS BLOB))) FROM agent_controls),0) AS bytes`).get();
    assert(bytes<=16*1024*1024,'Program evidence exceeds its bounded selection');
    const controls=Array.from(db.prepare('SELECT session_id,control_json FROM agent_controls ORDER BY session_id,seq').iterate(),
      row=>({session_id:row.session_id,record:JSON.parse(row.control_json)}));
    const facts=Array.from(db.prepare('SELECT session_id,fact_json,fact_frame,logical_bytes FROM facts ORDER BY session_id,seq').iterate(),
      row=>({session_id:row.session_id,record:storedFact(row)}));
    return {controls,facts};
  } finally {db.close();}
}

// Matches the Session UI's actual cancelling feedback, including its casing.
export function assertNoPendingWorkflowCleanup(text) {
  assert.doesNotMatch(text, /cleanup pending/i,
    'terminal readback must clear pending cleanup feedback');
}
