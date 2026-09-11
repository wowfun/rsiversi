import { useState } from 'react'
import { Button } from '../vendor/dsh/primitives/Button.tsx'
import { input, run, useView, type NavigationEntry } from './bridge.ts'
const basename = (path: string) => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path
export function Navigation() {
  const view = useView(value => value)
  const nav = view?.navigation
  const [query, setQuery] = useState(''), [archived, setArchived] = useState(false), [workspace, setWorkspace] = useState<string | null>(null)
  const [path, setPath] = useState(''), [trust, setTrust] = useState(false), [edit, setEdit] = useState<NavigationEntry>(), [title, setTitle] = useState('')
  const filter = {query, archived, workspace}
  const search = () => input.command({action: 'navigate', command: {kind: 'query', filter}})
  const mutate = (entry: NavigationEntry, metadata: NavigationEntry['metadata']) => input.command({action: 'navigate', command: {kind: 'replace', ticket: nav?.ticket, session: entry.session, metadata}})
  return <>
    <div className="section-heading"><h2>Workspaces</h2><Button id="refresh" size="sm" aria-label="Refresh workspaces and conversations" onClick={() => void run(async () => {await input.command({action:'refresh'}); await search()})}>↻</Button></div>
    <div id="workspaces" className="nav-list">{view?.catalog.workspaces.map(item => <div key={item.id} className="workspace-row"><button className="nav-item" title={item.path} onClick={() => void run(() => input.open({action:'create', workspace:item.id, trust}))}><strong>{basename(item.path)}</strong><small>{item.path}</small></button><Button size="sm" aria-label={`Filter ${basename(item.path)}`} onClick={() => {setWorkspace(item.id); void run(() => input.command({action:'navigate',command:{kind:'query',filter:{...filter,workspace:item.id}}}))}}>⌕</Button></div>)}</div>
    {!view?.catalog.workspaces.length && <p className="hint">Add a directory on your service to start.</p>}
    {view?.catalog.workspaces_more && <Button id="workspaces-next" size="sm" onClick={() => void run(() => input.command({action:'workspaces_next'}))}>More workspaces</Button>}
    <details className="workspace-add"><summary>Add workspace</summary><form id="workspace-form" className="workspace-form" onSubmit={event => {event.preventDefault(); void run(async () => {await input.command({action:'register_workspace',path}); setPath('')})}}><label htmlFor="workspace-path">Server directory</label><input id="workspace-path" value={path} onChange={e => setPath(e.target.value)} required placeholder="/path/to/project"/><Button type="submit" variant="outline" size="sm">Add workspace</Button></form></details>
    <label className="check trust"><input id="workspace-trust" type="checkbox" checked={trust} onChange={e => setTrust(e.target.checked)}/>Trust project instructions</label>
    <div className="section-heading conversations-heading"><h2>Conversations</h2><span className="count">{nav?.entries.length ?? 0}</span></div>
    <form className="navigation-search" onSubmit={e => {e.preventDefault(); void run(search)}}><input aria-label="Search conversations" placeholder="Search title or path" value={query} onChange={e => setQuery(e.target.value)}/><Button size="sm" type="submit" aria-label="Search">⌕</Button></form>
    <label className="check"><input aria-label="Show archived" type="checkbox" checked={archived} onChange={e => {const value=e.target.checked; setArchived(value); void run(() => input.command({action:'navigate',command:{kind:'query',filter:{...filter,archived:value}}}))}}/>Archived</label>
    {workspace && <Button size="sm" onClick={() => {setWorkspace(null); void run(() => input.command({action:'navigate',command:{kind:'query',filter:{...filter,workspace:null}}}))}}>All workspaces ×</Button>}
    {nav?.diagnostic && <p role="alert" className="settings-error">{nav.diagnostic}</p>}
    <div id="sessions" className="nav-list">{nav?.entries.map(entry => <div className="session-row" key={entry.session}><button className="nav-item" title={`${entry.path} · ${entry.session}`} onClick={() => void run(() => input.open({action:'open',session:entry.session}))}><strong>{entry.metadata.title ?? basename(entry.path)}</strong><small>{entry.workspace ? basename(entry.path) : 'Unregistered workspace'} · {entry.session}</small></button><details className="session-options"><summary aria-label={`Options for ${entry.session}`}>···</summary><Button size="sm" onClick={() => {setEdit(entry); setTitle(entry.metadata.title ?? '')}}>Rename</Button><Button size="sm" onClick={() => void run(() => mutate(entry,{...entry.metadata,archived:!entry.metadata.archived}))}>{entry.metadata.archived ? 'Unarchive' : 'Archive'}</Button></details></div>)}</div>
    {nav && !nav.entries.length && <p className="hint">{nav.more ? `Scanned ${nav.scanned} conversations. Continue searching.` : 'No matching conversations.'}</p>}
    {nav?.more && <Button id="sessions-next" size="sm" onClick={() => void run(() => input.command({action:'navigate',command:{kind:'next',ticket:nav.ticket}}))}>Continue conversations</Button>}
    {edit && <form className="rename-session" onSubmit={e => {e.preventDefault(); void run(async () => {await mutate(edit,{...edit.metadata,title:title || null}); setEdit(undefined)})}}><label>Conversation title<input aria-label="Conversation title" value={title} onChange={e => setTitle(e.target.value)} maxLength={256}/></label><Button type="submit" size="sm">Save title</Button><Button size="sm" onClick={() => setEdit(undefined)}>Cancel</Button></form>}
  </>
}
