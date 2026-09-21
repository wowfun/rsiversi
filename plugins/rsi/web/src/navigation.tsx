import { useState } from 'react'
import { Button } from '../vendor/dsh/primitives/Button.tsx'
import { input, run, useView, type NavigationEntry } from './bridge.ts'
const basename = (path: string) => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path
export function Navigation() {
  const view = useView(value => value)
  const nav = view?.navigation
  const [query, setQuery] = useState(''), [archived, setArchived] = useState(false), [workspace, setWorkspace] = useState<string | null>(null)
  const [path, setPath] = useState(''), [edit, setEdit] = useState<NavigationEntry>(), [title, setTitle] = useState('')
  const [pendingStarts,setPendingStarts]=useState<Record<string,string>>({})
  const external=view?.external_catalog
  const startExternal=async (endpoint:string)=>{
    const id=pendingStarts[endpoint]??`external_${crypto.randomUUID()}`
    setPendingStarts(value=>({...value,[endpoint]:id}))
    await input.open({action:'external_start',id,endpoint})
    setPendingStarts(value=>{const next={...value};delete next[endpoint];return next})
  }
  const filter = {query, archived, workspace}
  const search = () => input.command({action: 'navigate', command: {kind: 'query', filter}})
  const mutate = (entry: NavigationEntry, metadata: NavigationEntry['metadata']) => input.command({action: 'navigate', command: {kind: 'replace', ticket: nav?.ticket, session: entry.session, metadata}})
  return <>
    <section className="attention-navigation" aria-label="Needs attention"><div className="section-heading"><h2>Needs attention</h2><span className="count">{nav?.attention?.entries.length ?? 0}</span></div>
      {nav?.attention_notice && <p role="status" className="hint">{nav.attention_notice}</p>}
      <div className="nav-list">{nav?.attention?.entries.map(row=><div key={`${row.position.conversation.kind}:${row.position.conversation.id}`} className="attention-row">
        <button className="nav-item" onClick={()=>void run(()=>input.open({action:'attention_open',position:row.position,target:null}))}><strong>{row.status==='unread'?'New activity':row.status==='waiting'?'Waiting for you':row.status==='running'?'Running':'Unknown'}</strong><small>{row.position.conversation.kind} · {row.position.conversation.id}</small></button>
        {row.targets.map((target,index)=><Button key={index} size="sm" onClick={()=>void run(()=>input.open({action:'attention_open',position:row.position,target}))}>{target.kind==='native'&&target.request.kind==='question'?'Answer question':'Review permission'} {index+1}</Button>)}
      </div>)}</div>
      {nav?.attention?.truncated && <p className="hint">Showing bounded active conversations. Open a conversation to see all requests.</p>}
      {nav?.attention && !nav.attention.entries.length && <p className="hint">No conversations need attention.</p>}
    </section>
    <div className="section-heading"><h2>Workspaces</h2><Button id="refresh" size="sm" aria-label="Refresh workspaces and conversations" onClick={() => void run(async () => {await input.command({action:'refresh'}); await search()})}>↻</Button></div>
    <div id="workspaces" className="nav-list">{view?.catalog.workspaces.map(item => <div key={item.id} className="workspace-row"><button className="nav-item" title={item.path} onClick={() => void run(() => input.open({action:'create', workspace:item.id}))}><strong>{basename(item.path)}</strong><small>{item.path}</small></button><Button size="sm" aria-label={`Filter ${basename(item.path)}`} onClick={() => {setWorkspace(item.id); void run(() => input.command({action:'navigate',command:{kind:'query',filter:{...filter,workspace:item.id}}}))}}>⌕</Button></div>)}</div>
    {!view?.catalog.workspaces.length && <p className="hint">Add a directory on your service to start.</p>}
    {view?.catalog.workspaces_more && <Button id="workspaces-next" size="sm" onClick={() => void run(() => input.command({action:'workspaces_next'}))}>More workspaces</Button>}
    <details className="workspace-add"><summary>Add workspace</summary><form id="workspace-form" className="workspace-form" onSubmit={event => {event.preventDefault(); void run(async () => {await input.command({action:'register_workspace',path}); setPath('')})}}><label htmlFor="workspace-path">Server directory</label><input id="workspace-path" value={path} onChange={e => setPath(e.target.value)} required placeholder="/path/to/project"/><Button type="submit" variant="outline" size="sm">Add workspace</Button></form></details>
    <div className="section-heading conversations-heading"><h2>Conversations</h2><span className="count">{nav?.entries.length ?? 0}</span></div>
    <form className="navigation-search" onSubmit={e => {e.preventDefault(); void run(search)}}><input aria-label="Search conversations" placeholder="Search title or path" value={query} onChange={e => setQuery(e.target.value)}/><Button size="sm" type="submit" aria-label="Search">⌕</Button></form>
    <label className="check"><input aria-label="Show archived" type="checkbox" checked={archived} onChange={e => {const value=e.target.checked; setArchived(value); void run(() => input.command({action:'navigate',command:{kind:'query',filter:{...filter,archived:value}}}))}}/>Archived</label>
    {workspace && <Button size="sm" onClick={() => {setWorkspace(null); void run(() => input.command({action:'navigate',command:{kind:'query',filter:{...filter,workspace:null}}}))}}>All workspaces ×</Button>}
    {nav?.diagnostic && <p role="alert" className="settings-error">{nav.diagnostic}</p>}
    <div id="sessions" className="nav-list">{nav?.entries.map(entry => <div className="session-row" key={entry.session}><button className="nav-item" title={`${entry.path} · ${entry.session}`} onClick={() => void run(() => input.open({action:'open',session:entry.session}))}><strong>{entry.metadata.title ?? basename(entry.path)}</strong><small>{entry.workspace ? basename(entry.path) : 'Unregistered workspace'} · {entry.session}</small></button><details className="session-options"><summary aria-label={`Options for ${entry.session}`}>···</summary><Button size="sm" onClick={() => {setEdit(entry); setTitle(entry.metadata.title ?? '')}}>Rename</Button><Button size="sm" onClick={() => void run(() => mutate(entry,{...entry.metadata,archived:!entry.metadata.archived}))}>{entry.metadata.archived ? 'Unarchive' : 'Archive'}</Button></details></div>)}</div>
    {nav && !nav.entries.length && <p className="hint">{nav.more ? `Scanned ${nav.scanned} conversations. Continue searching.` : 'No matching conversations.'}</p>}
    {nav?.more && <Button id="sessions-next" size="sm" onClick={() => void run(() => input.command({action:'navigate',command:{kind:'next',ticket:nav.ticket}}))}>Continue conversations</Button>}
    {edit && <form className="rename-session" onSubmit={e => {e.preventDefault(); void run(async () => {await mutate(edit,{...edit.metadata,title:title || null}); setEdit(undefined)})}}><label>Conversation title<input aria-label="Conversation title" value={title} onChange={e => setTitle(e.target.value)} maxLength={256}/></label><Button type="submit" size="sm">Save title</Button><Button size="sm" onClick={() => setEdit(undefined)}>Cancel</Button></form>}
    {external && <section className="external-navigation" aria-label="External agents"><div className="section-heading"><h2>External agents</h2><Button size="sm" aria-label="Refresh external conversations" onClick={()=>void run(()=>input.command({action:'external_catalog',next:false}))}>↻</Button></div>
      {external.endpoints.filter(endpoint=>endpoint.enabled).map(endpoint=><Button key={endpoint.id} size="sm" onClick={()=>void run(()=>startExternal(endpoint.id))}>{pendingStarts[endpoint.id]?'Check start':'Start'} {endpoint.id}</Button>)}
      {!external.endpoints.some(endpoint=>endpoint.enabled) && <p className="hint">No external agents enabled in this Host Profile.</p>}
      <div className="nav-list">{external.conversations.map(item=><button className="nav-item" key={item.id} title={item.id} onClick={()=>void run(()=>input.open({action:'external_open',id:item.id}))}><strong>{item.endpoint}</strong><small>{basename(item.cwd)}</small></button>)}</div>
      {external.more && <Button size="sm" onClick={()=>void run(()=>input.command({action:'external_catalog',next:true}))}>More external conversations</Button>}
    </section>}
  </>
}
