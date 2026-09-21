import { useState } from 'react'
import { Button } from '../vendor/dsh/primitives/Button.tsx'
import { input, run, useView, useSelected, type McpServer, type PluginsView } from './bridge.ts'
import { ProfileLeaves } from './profile-leaves.tsx'
export function Plugins({loading=false}:{loading?:boolean}) {
  const view = useView(view => view?.plugins), [working,setBusy] = useState(false), [problem,setProblem] = useState<string|null>(null)
  const busy = loading || working
  const selected = useSelected(value => value)
  const agentPreset = useView(view => view?.surfaces[selected]?.agent_preset)
  const session = useView(view => view?.surfaces[selected]?.session)
  const header = useView(view => view?.surfaces[selected]?.header)
  const [preset,setPreset] = useState('')
  const page = view?.page
  const command = (command: Record<string,unknown>) => void (async () => {
    setBusy(true); setProblem(null)
    try {await input.command({action:'plugins',command})}
    catch (error) {setProblem(error instanceof Error ? error.message : String(error))}
    finally {setBusy(false)}
  })()
  const label = (value: string) => value.replaceAll('_',' ')
  return <section className="plugins-panel" aria-label="Plugins">
    {view?.leaves?.available && <ProfileLeaves view={view.leaves} busy={busy} command={value => command({kind:'leaves',command:value})}/>}
    <div className="actions"><h2>Plugins</h2><Button disabled={busy} onClick={() => command({kind:'refresh'})}>Refresh plugin status</Button></div>
    <div className="actions" aria-label="Plugin observation source">
      <Button disabled={busy} onClick={() => command({kind:'select',target:{kind:'host'}})}>Host plugins</Button>
      <Button disabled={busy || !agentPreset} onClick={() => command({kind:'select',target:{kind:'preset',id:agentPreset}})}>Current preset configuration</Button>
      <Button disabled={busy || !session || !header} onClick={() => command({kind:'select',target:{kind:'session',target:{session_id:session,header_key:header}}})}>Session resident generation</Button>
      <label>Preset identity<input value={preset} maxLength={255} onChange={event => setPreset(event.target.value)}/></label>
      <Button disabled={busy || !preset} onClick={() => command({kind:'select',target:{kind:'preset',id:preset}})}>Inspect preset</Button>
    </div>
    <p>Source: {view?.target.kind ?? 'host'}</p>
    {view?.guidance.map(message => <p role="status" className="hint" key={message}>{message}</p>)}
    <p className="hint">{view?.target.kind === 'session' ? 'Session rows show the pinned composition manifest. They do not read current per-instance lifecycle.' : 'Desired configuration and observed lifecycle are captured independently. Disabled configuration does not describe a still-retiring instance.'}</p>
    {problem && <p role="alert" className="settings-error">{problem}</p>}
    {view?.diagnostic && <p role="alert" className="settings-error">Plugin status unavailable: {view.diagnostic}</p>}
    {!page && !view?.diagnostic && <p role="status">Read current plugin status to begin.</p>}
    {view?.exa_available && <WebRetrieval view={view} busy={busy} command={command}/>}
    {view?.mcp_available && <section className="mcp-panel" aria-label="MCP connections">
      <div className="actions"><h3>MCP connections</h3><Button disabled={busy} onClick={() => void run(() => input.command({action:'settings_read',namespace:'rsi.mcp'}))}>Edit HTTP endpoints</Button><Button disabled={busy} onClick={() => command({kind:'mcp_status'})}>Read MCP status</Button><Button disabled={busy} onClick={() => command({kind:'mcp_refresh',server:null})}>Apply and refresh HTTP</Button></div>
      <p className="hint">Enable endpoints and select their tool names in HTTP settings. Save, then apply and refresh. New conversations use the verified catalog; existing conversations keep their saved definitions. All conversation tools share a 64-tool limit. Configure stdio in the Local Host Profile.</p>
      {view.mcp_notice && <p role="status" className="mcp-notice">{view.mcp_notice}</p>}
      {view.mcp && <><p className="mcp-readiness">{view.mcp.settings_pending ? 'Saved HTTP settings need to be applied. ' : ''}{view.mcp.fresh_ready ? 'MCP catalog verified.' : `MCP catalog unavailable: ${label(view.mcp.fresh_error ?? 'unknown')}.`}</p>
        {view.mcp.servers.length === 0 && <p>No MCP endpoints configured.</p>}
        {view.mcp.servers.map(server => <article className="plugin-row" key={server.id}>
          <h4>{server.id} <span className="hint">{server.transport === 'stdio' ? 'Local stdio · remote observation only' : 'HTTP'}</span></h4>
          <p>{server.enabled ? (server.ready ? 'Ready' : label(server.error ?? 'unverified')) : 'Disabled'} · Connection epoch {server.epoch}</p>
          <p className="hint">{server.last_verified_sha256 ? `Last verified catalog ${server.last_verified_sha256.slice(0,12)}` : 'No verified catalog yet'}</p>
          {server.transport === 'http' && <Button disabled={busy || !server.enabled} onClick={() => command({kind:'mcp_refresh',server:server.id})}>Refresh {server.id}</Button>}
          {server.tools.length > 0 && <details><summary>Verified tools · {server.tools.filter(tool => tool.selected).length} selected / {server.tools.length}</summary><ul>{server.tools.map(tool => <li key={tool.name}><code>{tool.name}</code> · {tool.selected ? 'selected' : 'not selected'}</li>)}</ul><p className="hint">Selection changes take effect after saving HTTP settings and refreshing the connection.</p></details>}
          {server.credential && <McpCredential key={`${server.id}:${server.credential.owner}/${server.credential.slot}`} server={server} view={view} busy={busy} command={command}/>}
        </article>)}
      </>}
    </section>}
    {page && <><p className="plugin-revisions">Desired {page.desired_revision} · Observed {page.observed_revision}<br/>{page.health && <>Profile {label(page.health)} · Watcher {label(page.watcher ?? 'inactive')}</>}{!page.health && <>{label(page.context.availability)}{page.context.preset_source && ` · ${label(page.context.preset_source)} preset root`}</>}{page.context.source_digest && <><br/>Source {page.context.source_digest.slice(0,12)}</>}</p>
      <div className="plugin-rows">{page.plugins.map(row => <article className="plugin-row" key={row.instance}>
        <h3>{row.instance}</h3><p className="hint">{label(row.origin)}{row.diagnostics.map(reason => ` · ${label(reason)}`)}</p><dl><dt>Desired</dt><dd>{row.desired_plugin ?? 'Removed'} · {row.enabled ? 'enabled' : 'disabled'}</dd><dt>{view?.target.kind === 'session' ? 'Pinned manifest' : 'Observed'}</dt><dd>{row.observed ? `${label(row.observed.state)} · ${row.observed.plugin}` : 'Not observed'}</dd></dl>
      </article>)}</div>
      <div className="actions"><Button disabled={busy || page.offset === 0} onClick={() => command({kind:'page',ticket:view.ticket,offset:Math.max(0,page.offset-32)})}>Previous plugin page</Button><span>{page.offset + Number(page.plugins.length > 0)}–{page.offset+page.plugins.length} of {page.total}</span><Button disabled={busy || page.next_offset === null} onClick={() => command({kind:'page',ticket:view.ticket,offset:page.next_offset})}>Next plugin page</Button></div>
    </>}
  </section>
}

function WebRetrieval({view,busy,command}:{view:PluginsView;busy:boolean;command:(command:Record<string,unknown>)=>void}) {
  const [secret,setSecret] = useState('')
  const status = view.exa_credential
  return <section className="mcp-panel" aria-label="Web retrieval">
    <div className="actions"><h3>Web retrieval</h3><Button disabled={busy} onClick={() => void run(() => input.command({action:'settings_read',namespace:'rsi.retrieval'}))}>Web retrieval settings</Button></div>
    <p className="hint">Fetch public pages and search Exa. Both tools start disabled. Enable them for new conversations; disabling takes effect on future calls immediately. Recorded sources stay available in conversation history.</p>
    <details className="mcp-credential"><summary>Exa search credential</summary>
      <p>{status ? `${status.availability.kind} · ${status.editable ? 'editable' : 'read only'}` : 'Read Exa credential status before changing it.'}</p>
      <Button disabled={busy} onClick={() => command({kind:'exa_status'})}>Read Exa credential status</Button>
      <label>Exa API key<input type="password" autoComplete="new-password" spellCheck={false} maxLength={64*1024} value={secret} disabled={busy || !status?.editable} onChange={event => setSecret(event.target.value)}/></label>
      <div className="actions"><Button disabled={busy || !status?.editable || !secret} onClick={() => {const value=secret;setSecret('');command({kind:'exa_set',secret:value})}}>Save Exa credential</Button><Button disabled={busy || !status?.editable} onClick={() => {setSecret('');command({kind:'exa_unset'})}}>Remove Exa credential</Button></div>
      <p className="hint">Saving or removing this credential does not submit a search or change which tools are enabled.</p>
    </details>
    {view.exa_notice && <p role="status">{view.exa_notice}</p>}
  </section>
}

function McpCredential({server,view,busy,command}:{server:McpServer;view:PluginsView;busy:boolean;command:(command:Record<string,unknown>)=>void}) {
  const [secret,setSecret] = useState('')
  const target = {server:server.id,reference:server.credential!}
  const observed = view.mcp_credential
  const current = observed?.target.server === target.server && observed.target.reference.owner === target.reference.owner && observed.target.reference.slot === target.reference.slot ? observed : undefined
  return <details className="mcp-credential"><summary>Credential · {target.reference.slot}</summary>
    <p>{current ? `${current.availability.kind} · ${current.editable ? 'editable' : 'read only'}` : 'Read credential status before changing it.'}</p>
    <div className="actions"><Button disabled={busy} onClick={() => command({kind:'mcp_credential_status',target})}>Read credential status for {server.id}</Button></div>
    <label>API key for {server.id}<input type="password" autoComplete="new-password" maxLength={64 * 1024} spellCheck={false} value={secret} disabled={busy || !current?.editable} onChange={event => setSecret(event.target.value)} /></label>
    <div className="actions"><Button disabled={busy || !current?.editable || !secret} onClick={() => {const value=secret;setSecret('');command({kind:'mcp_credential_set',target,secret:value})}}>Save credential for {server.id}</Button><Button disabled={busy || !current?.editable} onClick={() => {setSecret('');command({kind:'mcp_credential_unset',target})}}>Remove credential for {server.id}</Button></div>
    <p className="hint">Credential writes have their own result. Refresh the connection separately to verify access.</p>
  </details>
}
