import { useState, useRef, useLayoutEffect } from 'react'
import { Button } from './button.tsx'

type Kind = 'enable'|'disable'|'configuration'
type Principal = {kind:'local'}|{kind:'device'|'agent';id:string}
type GrantPrincipal = Exclude<Principal['kind'], 'agent'>
type Target = {root:string;profile:string;leaf:string}
type GrantScope = {kind:'profile';target:Target;operation:Kind}|{kind:'ssh_use'|'ssh_manage';target:string}|{kind:'ssh_stdio';target:string;server:string;credentials:{owner:string;slot:string}[]}
type Grant = {principal:Principal;scope:GrantScope}
const scopeLabel=(scope:GrantScope)=>scope.kind==='profile'?`${scope.target.profile} / ${scope.target.leaf} · ${label(scope.operation)}`:scope.kind==='ssh_stdio'?`SSH ${scope.target} · stdio ${scope.server} · ${scope.credentials.map(ref=>`${ref.owner}/${ref.slot}`).join(', ')||'no credentials'}`:`SSH ${scope.target} · ${scope.kind==='ssh_use'?'Use':'Manage target'}`
type Preview = {host_epoch:string;target:Target;operation:Kind;digest:string;source_digest:string;plugin:string;previous_enabled:boolean;enabled:boolean;effective_enabled:boolean}
type ChangeResult = {preview:Preview;outcome:{state:'pending'|'saved'|'failed'|'unknown';directory_synced?:boolean;application?:string;failure?:{kind:string;ancestor?:string}}}
export interface LeafView {
  available:boolean;can_grant:boolean;query:{profile:string|null;after:string|null}
  catalog:{principal:Principal;root:string|null;profiles:string[];leaves:{target:Target;plugin:string;enabled:boolean;effective_enabled:boolean;allowed:Kind[]}[];next:string|null}|null
  grants:{revision:string;scopes:Grant[]}|null
  preview:Preview|null;result:ChangeResult|null;notice:string|null
}
type Command = (value:Record<string,unknown>) => void
const label = (value:string) => value.replaceAll('_',' ')
const who = (principal:Principal) => principal.kind === 'local' ? 'Local' : `${principal.kind} ${principal.id}`

export function ProfileLeaves({view,busy,command}:{view:LeafView;busy:boolean;command:Command}) {
  const [selected,setSelected] = useState(''), [document,setDocument] = useState('')
  const panel = useRef<HTMLElement>(null), reviewAction = useRef<{button:HTMLButtonElement;commit:boolean} | null>(null)
  useLayoutEffect(() => {
    if (view.preview || !reviewAction.current) return
    const {button,commit}=reviewAction.current;reviewAction.current=null
    if (!button.isConnected && globalThis.document.activeElement===globalThis.document.body) {
      const destination = (commit && panel.current?.querySelector<HTMLElement>('[aria-label="Source change result"]')) || panel.current?.querySelector<HTMLElement>('select:not(:disabled)') || panel.current?.querySelector<HTMLElement>('button:not(:disabled)')
      destination?.focus()
    }
  },[view.preview,view.result])
  const catalog = view.catalog, leaf = catalog?.leaves.find(leaf => leaf.target.leaf === selected)
  const read = (profile:string|null,after:string|null=null) => command({kind:'read',query:{profile,after}})

  return <section ref={panel} className="mcp-panel profile-leaves" aria-label="Host Profile management">
    <div className="actions"><h3>Host Profiles</h3><Button disabled={busy} onClick={() => read(null)}>Browse Host Profiles</Button></div>
    <p className="hint">Review and save one plugin change in a writable Host Profile. Changes from this panel require an explicit grant. Results show application as observed when the call completed. Refresh current source and plugin status to inspect later changes.</p>
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
    {view.preview && <article className="plugin-row" aria-label={`Prepared ${view.preview.target.leaf}`}>
      <h4>Review {label(view.preview.operation)} · {view.preview.target.leaf}</h4><p>{view.preview.target.profile} · {view.preview.plugin}</p><dl><dt>Own state</dt><dd>{view.preview.previous_enabled ? 'Enabled' : 'Disabled'} → {view.preview.enabled ? 'Enabled' : 'Disabled'}</dd><dt>With ancestor groups</dt><dd>{view.preview.effective_enabled ? 'Enabled' : 'Disabled'}</dd><dt>Review digest</dt><dd><code>{view.preview.digest}</code></dd></dl>
      <div className="actions"><Button disabled={busy} onClick={event => {reviewAction.current={button:event.currentTarget,commit:true};command({kind:'commit',digest:view.preview!.digest})}}>Save reviewed change</Button><Button disabled={busy} onClick={event => {reviewAction.current={button:event.currentTarget,commit:false};command({kind:'discard'})}}>Dismiss review</Button></div>
    </article>}
    {view.result && <ResultCard result={view.result} busy={busy} command={command}/>}
    {view.can_grant && <details><summary>Explicit Profile grants</summary><Button disabled={busy} onClick={() => command({kind:'grants'})}>Read Profile grants</Button>{view.grants?.scopes.map((scope,index) => <p key={index}>{who(scope.principal)} · {scopeLabel(scope.scope)} <Button disabled={busy} onClick={() => command({kind:'grant',revision:view.grants!.revision,scope,granted:false})}>Revoke grant {index+1}</Button></p>)}</details>}
  </section>
}
function GrantEditor({target,view,busy,command}:{target:Target;view:LeafView;busy:boolean;command:Command}) {
  const [principal,setPrincipal] = useState<GrantPrincipal>('local'), [id,setId] = useState(''), [operation,setOperation] = useState<Kind>('disable')
  return <details><summary>Grant an exact change</summary><p className="hint">Only the Local operator can grant a human client this operation on this leaf. Agents use Full access or approval for each call.</p><Button disabled={busy} onClick={() => command({kind:'grants'})}>Read grant revision</Button><label>Principal<select aria-label="Principal" value={principal} onChange={event => setPrincipal(event.target.value as GrantPrincipal)}><option value="local">Local</option><option value="device">Device</option></select></label>{principal === 'device' && <label>Exact Device ID<input value={id} maxLength={256} onChange={event => setId(event.target.value)}/></label>}<label>Allowed change<select aria-label="Allowed change" value={operation} onChange={event => setOperation(event.target.value as Kind)}><option value="disable">Disable</option><option value="enable">Enable</option><option value="configuration">Replace configuration</option></select></label><Button disabled={busy || !view.grants || (principal === 'device' && !id)} onClick={() => command({kind:'grant',revision:view.grants!.revision,scope:{principal:principal === 'local' ? {kind:'local'} : {kind:principal,id},scope:{kind:'profile',target,operation}},granted:true})}>Grant this exact change</Button></details>
}
function ResultCard({result,busy,command}:{result:ChangeResult;busy:boolean;command:Command}) {
  const outcome = result.outcome
  return <article className="plugin-row" aria-label="Source change result" tabIndex={-1}><h4>Last change result · {label(result.preview.operation)} · {result.preview.target.leaf}</h4><p>{result.preview.target.profile}</p><dl><dt>Source</dt><dd>{label(outcome.state)}</dd>{outcome.state === 'saved' && <><dt>Directory durability</dt><dd>{outcome.directory_synced ? 'Synced' : 'Not confirmed'}</dd><dt>Runtime at completion</dt><dd>{label(outcome.application ?? 'unknown')}</dd></>}{outcome.failure && <><dt>Reason</dt><dd>{label(outcome.failure.kind)}{outcome.failure.ancestor && `: ${outcome.failure.ancestor}`}</dd></>}</dl>{outcome.state === 'unknown' && <p>Current state cannot prove the result of the interrupted call.</p>}<Button disabled={busy} onClick={() => command({kind:'read',query:{profile:result.preview.target.profile,after:null}})}>Refresh current source</Button></article>
}
