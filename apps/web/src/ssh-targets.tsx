import { useEffect, useRef, useState } from 'react'
import { Button } from './button.tsx'
import type { Coordinates } from './bridge.ts'

type Candidate = {target:string;name:string;endpoint:{host:string;port:number;user:string}}
type Target = {candidate:Candidate;revision:string;fingerprint:string|null;connection_epoch:string|null;connected:boolean;unavailable_programs:string[];permissions:{use_target:boolean;manage:boolean}}
export type SshView = {available:boolean;can_trust:boolean;catalog:{host_epoch:string;targets:Target[]}|null;directory:Coordinates|null;uncertain:boolean;notice:string|null}
type Command = (value:Record<string,unknown>)=>void
export function SshTargets({view,busy,command,grantCommand,grantRevision}:{view:SshView;busy:boolean;command:Command;grantCommand:Command;grantRevision:string|null}) {
  const [name,setName] = useState(''), [host,setHost] = useState(''), [user,setUser] = useState(''), [port,setPort] = useState('22')
  const [identity,setIdentity] = useState(() => crypto.randomUUID().replaceAll('-',''))
  const candidateForm=useRef<HTMLDetailsElement>(null)
  const created=!!view.catalog?.targets.some(target=>target.candidate.target===identity)
  useEffect(()=>{if(created && candidateForm.current)candidateForm.current.open=false},[created])
  const locked = busy || view.uncertain, catalog = view.catalog
  const put = () => command({kind:'put',request:{host_epoch:catalog!.host_epoch,expected:'0',candidate:{target:identity,name,endpoint:{host,port:Number(port),user}}}})
  return <section className="mcp-panel ssh-targets" aria-label="SSH targets">
    <div className="actions"><h3>SSH targets</h3><Button disabled={busy} onClick={() => command({kind:'read'})}>Refresh SSH targets</Button></div>
    <p className="hint">Run workspace tools on a trusted Linux target. Conversations and model credentials stay on this Service.</p>
    {view.notice && <p role="status">{view.notice}</p>}
    {catalog && <>
      <details ref={candidateForm}><summary>Add an SSH target</summary>
        <label>Target name<input maxLength={128} value={name} onChange={event=>setName(event.target.value)}/></label>
        <label>Hostname or IP address<input maxLength={253} value={host} onChange={event=>setHost(event.target.value)}/></label>
        <label>SSH account<input maxLength={64} value={user} onChange={event=>setUser(event.target.value)}/></label>
        <label>SSH port<input type="number" min={1} max={65535} value={port} onChange={event=>setPort(event.target.value)}/></label>
        <Button disabled={locked || !name.trim() || !host || !user || !Number.isInteger(Number(port)) || Number(port)<1 || Number(port)>65535 || catalog.targets.some(target=>target.candidate.target===identity)} onClick={put}>Submit target candidate</Button>
        {catalog.targets.some(target=>target.candidate.target===identity) && <Button disabled={locked} onClick={()=>{setIdentity(crypto.randomUUID().replaceAll('-',''));setName('');setHost('');setUser('')}}>Add another target</Button>}
        <p className="hint">A Local operator must confirm the host key and authentication identity, then grant this device Use.</p>
      </details>
      {catalog.targets.map(target=><TargetCard key={target.candidate.target} target={target} epoch={catalog.host_epoch} local={view.can_trust} busy={locked} command={command} grantCommand={grantCommand} grantRevision={grantRevision}/>)}
      {!catalog.targets.length && <p>No SSH targets are visible to this connection.</p>}
    </>}
    {view.directory && <p role="status">Resolved target directory: <code>{view.directory.path}</code></p>}
  </section>
}
function TargetCard({target,epoch,local,busy,command,grantCommand,grantRevision}:{target:Target;epoch:string;local:boolean;busy:boolean;command:Command;grantCommand:Command;grantRevision:string|null}) {
  const [path,setPath]=useState(''), [editName,setEditName]=useState(target.candidate.name), [editHost,setEditHost]=useState(target.candidate.endpoint.host), [editUser,setEditUser]=useState(target.candidate.endpoint.user), [editPort,setEditPort]=useState(String(target.candidate.endpoint.port))
  const selection={host_epoch:epoch,target:target.candidate.target,revision:target.revision}, connection={selection,expected_connection_epoch:target.connection_epoch}
  return <article className="plugin-row" aria-label={`SSH target ${target.candidate.name}`}>
    <h4>{target.candidate.name}</h4><p>{target.candidate.endpoint.user}@{target.candidate.endpoint.host}:{target.candidate.endpoint.port}</p>
    <dl><dt>Trust</dt><dd>{target.fingerprint ? <code>{target.fingerprint}</code> : 'Awaiting Local confirmation'}</dd><dt>Connection</dt><dd>{target.connected ? 'Connected' : target.connection_epoch ? 'Connection lost' : 'Disconnected'}</dd><dt>Permissions</dt><dd>{[target.permissions.use_target && 'Use',target.permissions.manage && 'Manage'].filter(Boolean).join(', ') || 'No execution permission'}</dd></dl>
    {!!target.unavailable_programs.length && <p>Unavailable target programs: {target.unavailable_programs.join(', ')}</p>}
    <div className="actions"><Button disabled={busy || !target.permissions.use_target || !target.fingerprint} onClick={()=>command({kind:'connect',request:connection})}>{target.connection_epoch ? 'Reconnect target' : 'Connect target'}</Button><Button disabled={busy || !target.permissions.manage || !target.connection_epoch} onClick={()=>command({kind:'disconnect',request:connection})}>Disconnect target</Button></div>
    {target.connected && target.permissions.use_target && <div><label>Target directory<input maxLength={16*1024} value={path} placeholder="/home/account/project" onChange={event=>setPath(event.target.value)}/></label><Button disabled={busy || !path.startsWith('/')} onClick={()=>command({kind:'resolve',request:{connection,path}})}>Check target directory</Button></div>}
    {target.permissions.manage && <details><summary>Edit target</summary><label>Target label<input maxLength={128} value={editName} onChange={event=>setEditName(event.target.value)}/></label><label>Target address<input maxLength={253} value={editHost} onChange={event=>setEditHost(event.target.value)}/></label><label>Target account<input maxLength={64} value={editUser} onChange={event=>setEditUser(event.target.value)}/></label><label>Target port<input type="number" min={1} max={65535} value={editPort} onChange={event=>setEditPort(event.target.value)}/></label><p className="hint">Saving requires a fresh Local trust confirmation before reconnecting.</p><Button disabled={busy || !editName.trim() || !editHost || !editUser || !Number.isInteger(Number(editPort)) || Number(editPort)<1 || Number(editPort)>65535} onClick={()=>command({kind:'put',request:{host_epoch:epoch,expected:target.revision,candidate:{target:target.candidate.target,name:editName,endpoint:{host:editHost,port:Number(editPort),user:editUser}}}})}>Save target candidate</Button></details>}
    {local && <LocalTrust selection={selection} busy={busy} command={command}/>}
    {local && <TargetGrant target={target.candidate.target} busy={busy} command={grantCommand} revision={grantRevision}/>}
  </article>
}
function LocalTrust({selection,busy,command}:{selection:{host_epoch:string;target:string;revision:string};busy:boolean;command:Command}) {
  const [key,setKey]=useState(''), [fingerprint,setFingerprint]=useState(''), [identity,setIdentity]=useState('')
  const parts=key.trim().split(/\s+/)
  return <details><summary>Confirm trust as Local operator</summary><label>Verified host public key<textarea rows={2} maxLength={4096} spellCheck={false} value={key} onChange={event=>setKey(event.target.value)}/></label><label>Verified SHA256 fingerprint<input maxLength={50} spellCheck={false} value={fingerprint} placeholder="SHA256:…" onChange={event=>setFingerprint(event.target.value)}/></label><label>Private identity file on this Service<input maxLength={16*1024} value={identity} autoComplete="off" onChange={event=>setIdentity(event.target.value)}/></label><p className="hint">Confirm the fingerprint through a trusted channel. Use a private, unencrypted identity file available to the Service.</p><Button disabled={busy || parts.length<2 || !fingerprint.startsWith('SHA256:') || !identity.startsWith('/')} onClick={()=>{command({kind:'trust',request:{selection,host_key:{algorithm:parts[0],base64:parts[1]},fingerprint,identity_path:identity}});setIdentity('')}}>Confirm host key and identity</Button></details>
}
function TargetGrant({target,busy,command,revision}:{target:string;busy:boolean;command:Command;revision:string|null}) {
  const [device,setDevice]=useState(''), [kind,setKind]=useState('ssh_use')
  return <details><summary>Grant this device target access</summary><Button disabled={busy} onClick={()=>command({kind:'grants'})}>Read grant revision</Button><label>Device ID<input value={device} maxLength={32} onChange={event=>setDevice(event.target.value)}/></label><label>Target permission<select value={kind} onChange={event=>setKind(event.target.value)}><option value="ssh_use">Use target</option><option value="ssh_manage">Manage target</option></select></label><Button disabled={busy || !revision || !/^[a-f0-9]{32}$/.test(device)} onClick={()=>command({kind:'grant',revision,scope:{principal:{kind:'device',id:device},scope:{kind,target}},granted:true})}>Grant exact target permission</Button></details>
}
