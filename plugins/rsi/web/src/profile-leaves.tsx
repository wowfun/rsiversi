import { useState } from 'react'
import { Button } from '../vendor/dsh/primitives/Button.tsx'

type Kind = 'enable'|'disable'|'configuration'
type Principal = {kind:'local'}|{kind:'device'|'agent';id:string}
type Target = {root:string;profile:string;leaf:string}
type Grant = {principal:Principal;target:Target;operation:Kind}
type Preview = {host_epoch:string;ticket:string;target:Target;operation:Kind;digest:string;source_digest:string;plugin:string;previous_enabled:boolean;enabled:boolean;effective_enabled:boolean}
type Receipt = {preview:Preview;outcome:{state:'pending'|'saved'|'failed'|'unknown';directory_synced?:boolean;application?:string;failure?:{kind:string;ancestor?:string}}}
export interface LeafView {
  available:boolean;can_grant:boolean;query:{profile:string|null;after:string|null}
  catalog:{principal:Principal;root:string|null;profiles:string[];leaves:{target:Target;plugin:string;enabled:boolean;effective_enabled:boolean;allowed:Kind[]}[];next:string|null}|null
  grants:{revision:string;scopes:Grant[]}|null
  previews:Preview[];receipts:string[];receipt:Receipt|null;notice:string|null
}
type Command = (value:Record<string,unknown>) => void
const label = (value:string) => value.replaceAll('_',' ')
const who = (principal:Principal) => principal.kind === 'local' ? 'Local' : `${principal.kind} ${principal.id}`

export function ProfileLeaves({view,busy,command}:{view:LeafView;busy:boolean;command:Command}) {
  const [selected,setSelected] = useState(''), [document,setDocument] = useState(''), [receipt,setReceipt] = useState('')
  const catalog = view.catalog, leaf = catalog?.leaves.find(leaf => leaf.target.leaf === selected)
  const read = (profile:string|null,after:string|null=null) => command({kind:'read',query:{profile,after}})
  const uncertain = (ticket:string) => view.receipt?.preview.ticket === ticket && ['pending','unknown'].includes(view.receipt.outcome.state)
  return <section className="mcp-panel profile-leaves" aria-label="Host Profile management">
    <div className="actions"><h3>Host Profiles</h3><Button disabled={busy} onClick={() => read(null)}>Browse Host Profiles</Button></div>
    <p className="hint">Review and save one plugin change in a writable Host Profile. Each change requires an explicit grant. Existing conversation generations keep their current plugins.</p>
    {catalog && <>
      <p>Caller: {who(catalog.principal)}{catalog.root && <> · Source root <code>{catalog.root.slice(0,12)}</code></>}</p>
      {!view.query.profile && <div className="actions">{catalog.profiles.map(profile => <Button key={profile} disabled={busy} onClick={() => {setSelected('');read(profile)}}>Open Profile {profile}</Button>)}{!catalog.profiles.length && <p>No writable Host Profiles on this page.</p>}</div>}
      {view.query.profile && <>
        <div className="actions"><h4>Profile {view.query.profile}</h4><Button disabled={busy} onClick={() => read(view.query.profile)}>Refresh source choices</Button></div>
        <label>Plugin leaf<select aria-label="Plugin leaf" value={leaf?.target.leaf ?? ''} disabled={busy} onChange={event => {setSelected(event.target.value);setDocument('')}}><option value="">Select a leaf on this page</option>{catalog.leaves.map(leaf => <option key={leaf.target.leaf} value={leaf.target.leaf}>{leaf.target.leaf} · {leaf.enabled ? 'enabled' : 'disabled'}</option>)}</select></label>
        {leaf && <article className="plugin-row">
          <h4>{leaf.target.leaf}</h4><p>{leaf.plugin}</p><dl><dt>Own state</dt><dd>{leaf.enabled ? 'Enabled' : 'Disabled'}</dd><dt>With ancestor groups</dt><dd>{leaf.effective_enabled ? 'Enabled' : 'Disabled'}</dd><dt>Granted changes</dt><dd>{leaf.allowed.map(label).join(', ') || 'None'}</dd></dl>
          <div className="actions"><Button disabled={busy || !leaf.allowed.includes('enable')} onClick={() => command({kind:'enabled',target:leaf.target,enabled:true})}>Preview enable</Button><Button disabled={busy || !leaf.allowed.includes('disable')} onClick={() => command({kind:'enabled',target:leaf.target,enabled:false})}>Preview disable</Button></div>
          <details><summary>Replace configuration</summary><label>Complete JSON configuration<textarea rows={6} spellCheck={false} value={document} disabled={busy} maxLength={64*1024} onChange={event => setDocument(event.target.value)}/></label><p className="hint">This replaces the complete configuration. Values stay hidden in the review; Rust preserves exact JSON numbers and nulls.</p><Button disabled={busy || !document || !leaf.allowed.includes('configuration')} onClick={() => command({kind:'configuration',target:leaf.target,document})}>Prepare configuration replacement</Button></details>
          {view.can_grant && <GrantEditor target={leaf.target} view={view} busy={busy} command={command}/>}
        </article>}
      </>}
      {catalog.next && <Button disabled={busy} onClick={() => {setSelected('');read(view.query.profile,catalog.next)}}>Next source page</Button>}
    </>}
    {view.notice && <p role="status">{view.notice}</p>}
    {view.previews.map(preview => <article className="plugin-row" key={preview.ticket} aria-label={`Prepared ${preview.target.leaf}`}>
      <h4>Review {label(preview.operation)} · {preview.target.leaf}</h4><p>{preview.target.profile} · {preview.plugin}</p><dl><dt>Own state</dt><dd>{preview.previous_enabled ? 'Enabled' : 'Disabled'} → {preview.enabled ? 'Enabled' : 'Disabled'}</dd><dt>With ancestor groups</dt><dd>{preview.effective_enabled ? 'Enabled' : 'Disabled'}</dd><dt>Review digest</dt><dd><code>{preview.digest}</code></dd><dt>Ticket</dt><dd><code>{preview.ticket}</code></dd></dl>
      <div className="actions"><Button disabled={busy || uncertain(preview.ticket)} onClick={() => command({kind:'commit',ticket:preview.ticket,digest:preview.digest})}>Save reviewed change</Button><Button disabled={busy || uncertain(preview.ticket)} onClick={() => command({kind:'discard',ticket:preview.ticket})}>Discard proposal</Button></div>
    </article>)}
    {view.receipt && <ReceiptCard receipt={view.receipt} busy={busy} command={command}/>}
    {!!view.receipts.length && <details><summary>Recover a source receipt ({view.receipts.length})</summary><label>Original receipt ticket<select aria-label="Original receipt ticket" value={receipt} onChange={event => setReceipt(event.target.value)}><option value="">Select an owned receipt</option>{view.receipts.map(ticket => <option key={ticket}>{ticket}</option>)}</select></label><Button disabled={busy || !receipt} onClick={() => command({kind:'receipt',ticket:receipt})}>Read selected receipt</Button></details>}
    {view.can_grant && <details><summary>Explicit Profile grants</summary><Button disabled={busy} onClick={() => command({kind:'grants'})}>Read Profile grants</Button>{view.grants?.scopes.map((scope,index) => <p key={index}>{who(scope.principal)} · {scope.target.profile} / {scope.target.leaf} · {label(scope.operation)} <Button disabled={busy} onClick={() => command({kind:'grant',revision:view.grants!.revision,scope,granted:false})}>Revoke grant {index+1}</Button></p>)}</details>}
  </section>
}
function GrantEditor({target,view,busy,command}:{target:Target;view:LeafView;busy:boolean;command:Command}) {
  const [principal,setPrincipal] = useState<Principal['kind']>('local'), [id,setId] = useState(''), [operation,setOperation] = useState<Kind>('disable')
  return <details><summary>Grant an exact change</summary><p className="hint">Only the Local operator can grant a principal this operation on this leaf.</p><Button disabled={busy} onClick={() => command({kind:'grants'})}>Read grant revision</Button><label>Principal<select aria-label="Principal" value={principal} onChange={event => setPrincipal(event.target.value as Principal['kind'])}><option value="local">Local</option><option value="device">Device</option><option value="agent">Agent Session</option></select></label>{principal !== 'local' && <label>Exact {principal === 'device' ? 'Device' : 'Session'} ID<input value={id} maxLength={256} onChange={event => setId(event.target.value)}/></label>}<label>Allowed change<select aria-label="Allowed change" value={operation} onChange={event => setOperation(event.target.value as Kind)}><option value="disable">Disable</option><option value="enable">Enable</option><option value="configuration">Replace configuration</option></select></label><Button disabled={busy || !view.grants || (principal !== 'local' && !id)} onClick={() => command({kind:'grant',revision:view.grants!.revision,scope:{principal:principal === 'local' ? {kind:'local'} : {kind:principal,id},target,operation},granted:true})}>Grant this exact change</Button></details>
}
function ReceiptCard({receipt,busy,command}:{receipt:Receipt;busy:boolean;command:Command}) {
  const outcome = receipt.outcome
  return <article className="plugin-row" aria-label="Profile source receipt"><h4>Source receipt · {receipt.preview.target.leaf}</h4><p>{receipt.preview.target.profile} · <code>{receipt.preview.ticket}</code></p><dl><dt>Source</dt><dd>{label(outcome.state)}</dd>{outcome.state === 'saved' && <><dt>Directory durability</dt><dd>{outcome.directory_synced ? 'Synced' : 'Not confirmed'}</dd><dt>Current runtime</dt><dd>{label(outcome.application ?? 'unknown')}</dd></>}{outcome.failure && <><dt>Reason</dt><dd>{label(outcome.failure.kind)}{outcome.failure.ancestor && `: ${outcome.failure.ancestor}`}</dd></>}</dl><Button disabled={busy} onClick={() => command({kind:'receipt',ticket:receipt.preview.ticket})}>Query original receipt</Button></article>
}
