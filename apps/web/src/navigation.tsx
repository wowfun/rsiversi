import {useLayout} from './presentation.tsx'
import {workspacePreference} from '../presentation-store.js'
import { useEffect, useMemo, useRef, useState } from 'react'
import {chooseWorkspace} from './directory-picker.tsx'
import { Button } from './button.tsx'
import { input, run, useView, type NavigationEntry, type WorkspaceFilter } from './bridge.ts'
import { navigationPresentation, GroupRestoration } from '../navigation-presentation.js'
const basename = (path: string) => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path
export function Navigation() {
  const view = useView(value => value), nav = view?.navigation
  const {layout,update}=useLayout()
  const restoring=useRef(new GroupRestoration())
  const desired=new Set(layout.workspaces.filter(item=>item.expanded).map(item=>item.id))
  for(const surface of Object.values(view?.surfaces??{})){
    const workspace=view?.catalog.workspaces.find(row=>row.path===surface?.path)
    if(workspace&&!layout.workspaces.some(item=>item.id===workspace.id&&!item.expanded))desired.add(workspace.id)
  }
  const restoreKey=JSON.stringify([...desired].filter(id=>!nav?.groups[id]&&(id==='other'||view?.catalog.workspaces.some(row=>row.id===id))).sort())
  useEffect(()=>{
    if(!nav?.ticket)return
    for(const id of JSON.parse(restoreKey) as string[]){
      const workspace=id==='other'?{kind:'unregistered'}:{kind:'registered',id}
      void run(()=>restoring.current.run(id,()=>input.command({action:'navigate',command:{kind:'group',workspace,ticket:null}})))
    }
  },[restoreKey,nav?.ticket])
  const [searchOpen,setSearchOpen]=useState(false),[filterOpen,setFilterOpen]=useState(false)
  const [query,setQuery] = useState(''), [mode,setMode] = useState('all')
  const [path,setPath] = useState(''), [edit,setEdit] = useState<NavigationEntry>(), [title,setTitle] = useState('')
  const [pendingStarts,setPendingStarts] = useState<Record<string,string>>({})
  const external = view?.external_catalog
  const filter = {query, archived:mode==='archived', workspace:{kind:'all'}}
  const search = (value=filter) => input.command({action:'navigate',command:{kind:'query',filter:value}})
  const mutate = (session:string, metadata:NavigationEntry['metadata']) => input.command({action:'navigate',command:{kind:'replace',ticket:nav?.ticket,session,metadata}})
  const presentation=useMemo(()=>navigationPresentation([...(nav?.entries??[]),...Object.values(nav?.groups??{}).flatMap(group=>group.entries),...(nav?.pins??[]).flatMap(pin=>pin.status==='available'?[pin.entry]:[])],nav?.attention),[nav?.entries,nav?.groups,nav?.pins,nav?.attention])
  const row = (entry:NavigationEntry) => {
    const activity=presentation.attention.get(entry.session)??'unknown'
    return <div className="session-row" data-testid="conversation-row" data-session-id={entry.session} key={entry.session}>
    <button className="nav-item" data-testid="conversation-open" title={`${entry.path}\n${entry.session}`} onClick={()=>void run(()=>input.open({action:'open',session:entry.session}))}>
      <span className={`session-state ${activity}`} aria-label={`Activity: ${activity}`}/>
      <strong>{presentation.titles.get(entry.session)}</strong>
    </button>
    <details className="session-options"><summary aria-label={`Options for ${entry.session}`}>···</summary>
      <Button size="sm" onClick={()=>void run(()=>mutate(entry.session,{...entry.metadata,pinned:!entry.metadata.pinned}))} disabled={entry.metadata.archived}>{entry.metadata.pinned?'Unpin':'Pin'}</Button>
      <Button size="sm" onClick={()=>{setEdit(entry);setTitle(entry.metadata.title??'')}}>Rename</Button>
      <Button size="sm" onClick={()=>void run(()=>mutate(entry.session,{...entry.metadata,pinned:false,archived:!entry.metadata.archived}))}>{entry.metadata.archived?'Unarchive':'Archive'}</Button>
      <p className="hint">{entry.path}<br/>{entry.session}</p>
    </details>
  </div>
  }
  const group = (key:string,name:string,workspace:WorkspaceFilter,path?:string) => {
    const page = nav?.groups[key]
    return <section className="workspace-group" key={key} data-testid="workspace-group" data-workspace-id={key}>
      <div className="workspace-row">
        <button className="nav-item workspace-toggle" aria-expanded={!!page} title={path} onClick={()=>void run(async()=>{await input.command({action:'navigate',command:page?{kind:'close_group',workspace}:{kind:'group',workspace,ticket:null}});update(workspacePreference(layout,key,!page))})}><span aria-hidden="true">{page?'⌄':'›'}</span><strong>{name}</strong></button>
        {path&&<Button size="sm" aria-label={`New conversation in ${name}`} data-testid="workspace-open" onClick={()=>void run(()=>input.open({action:'create',workspace:key}))}>+</Button>}
      </div>
      {page&&<div className="workspace-conversations">{page.continued&&!page.stale&&<p className="hint">Earlier entries are outside this page. <button onClick={()=>void run(()=>input.command({action:'navigate',command:{kind:'group',workspace,ticket:null}}))}>Back to newest</button></p>}{page.entries.map(row)}
        {page.stale&&<p className="hint" role="status">Conversations changed. <button aria-label={`Refresh conversations in ${name}`} onClick={()=>void run(()=>input.command({action:'navigate',command:{kind:'group',workspace,ticket:null}}))}>Refresh this workspace</button></p>}
        {!page.stale&&!page.entries.length&&<p className="hint">{page.more?'No matches in this scan. Continue searching.':'No conversations yet.'}</p>}
        {page.more&&<Button size="sm" aria-label={`Load more in ${name}`} onClick={()=>void run(()=>input.command({action:'navigate',command:{kind:'group',workspace,ticket:page.ticket}}))}>Load more</Button>}
      </div>}
    </section>
  }
  const startExternal = async (endpoint:string) => {
    const id=pendingStarts[endpoint]??`external_${crypto.randomUUID()}`
    setPendingStarts(value=>({...value,[endpoint]:id}))
    await input.open({action:'external_start',id,endpoint})
    setPendingStarts(value=>{const next={...value};delete next[endpoint];return next})
  }
  return <>
    <div className="section-heading"><h2>Workspaces</h2><div className="navigation-actions"><Button size="sm" aria-label="Search conversations" aria-expanded={searchOpen} onClick={()=>setSearchOpen(!searchOpen)}>⌕</Button><Button size="sm" aria-label="Filter conversations" aria-expanded={filterOpen} onClick={()=>setFilterOpen(!filterOpen)}>☷</Button><Button size="sm" data-testid="choose-workspace" aria-label="Choose workspace" onClick={chooseWorkspace}>+</Button><Button id="refresh" size="sm" aria-label="Refresh workspaces and conversations" onClick={()=>void run(async()=>{await input.command({action:'refresh'});await search()})}>↻</Button></div></div>
    <form hidden={!searchOpen} className="navigation-search" onSubmit={event=>{event.preventDefault();void run(()=>search())}}><input aria-label="Search conversations" placeholder="Search conversations" value={query} onChange={event=>setQuery(event.target.value)}/><Button size="sm" type="submit" aria-label="Search">⌕</Button></form>
    <select hidden={!filterOpen} aria-label="Conversation filter" value={mode} onChange={event=>{const value=event.target.value;setMode(value);if(value!=='attention')void run(()=>search({...filter,archived:value==='archived'}))}}><option value="all">All conversations</option><option value="attention">Needs attention</option><option value="archived">Archived</option></select>
    {nav?.diagnostic&&<p role="alert" className="settings-error">{nav.diagnostic}</p>}
    {mode==='attention'?<section className="attention-navigation" aria-label="Needs attention">
      {nav?.attention_notice&&<p role="status" className="hint">{nav.attention_notice}</p>}
      {nav?.attention?.entries.map(entry=><div className="attention-row" key={`${entry.position.conversation.kind}:${entry.position.conversation.id}`}>
        <button className="nav-item" onClick={()=>void run(()=>input.open({action:'attention_open',position:entry.position,target:null}))}><strong>{entry.status==='waiting'?'Waiting for you':entry.status==='unread'?'New activity':entry.status==='running'?'Running':'Unknown'}</strong><small>{entry.position.conversation.id}</small></button>
        {entry.targets.map((target,index)=><Button size="sm" key={index} onClick={()=>void run(()=>input.open({action:'attention_open',position:entry.position,target}))}>{target.kind==='native'&&target.request.kind==='question'?'Answer question':'Review permission'} {index+1}</Button>)}
      </div>)}
      {nav?.attention?.truncated&&<p className="hint">Only the bounded active-conversation window is shown.</p>}
      {nav?.attention&&!nav.attention.entries.length&&<p className="hint">No conversations in the current attention window.</p>}
    </section>:<>
      {!!nav?.pins.length&&<section className="pinned-conversations" aria-label="Pinned conversations"><h3>Pinned</h3>{nav.pins.map(pin=>pin.status==='available'?row(pin.entry):<div className="session-row stale" key={pin.session}><span>{pin.metadata.title??pin.session} — unavailable</span><Button size="sm" onClick={()=>void run(()=>mutate(pin.session,{...pin.metadata,pinned:false}))}>Unpin</Button></div>)}</section>}
      <div id="sessions" className="nav-list">{query||mode==='archived'?<>
        {nav?.entries.map(row)}{nav&&!nav.entries.length&&<p className="hint">{nav.more?'Continue searching the remaining conversations.':'No matching conversations.'}</p>}
        {nav?.more&&<Button id="sessions-next" size="sm" onClick={()=>void run(()=>input.command({action:'navigate',command:{kind:'next',ticket:nav.ticket}}))}>Continue conversations</Button>}
      </>:<><div id="workspaces">{view?.catalog.workspaces.map(item=>group(item.id,basename(item.path),{kind:'registered',id:item.id},item.path))}</div>
        {group('other','Other conversations',{kind:'unregistered'})}
        {!view?.catalog.workspaces.length&&<p className="hint">Choose a workspace to start.</p>}
        {view?.catalog.workspaces_more&&<Button id="workspaces-next" size="sm" onClick={()=>void run(()=>input.command({action:'workspaces_next'}))}>More workspaces</Button>}
      </>}</div>
    </>}
    <details className="workspace-add"><summary>Add workspace</summary><form id="workspace-form" className="workspace-form" onSubmit={event=>{event.preventDefault();void run(async()=>{await input.command({action:'register_workspace',path});setPath('')})}}><label htmlFor="workspace-path">Server directory</label><input id="workspace-path" value={path} onChange={event=>setPath(event.target.value)} required placeholder="/path/to/project"/><Button type="submit" variant="outline" size="sm">Add workspace</Button></form></details>
    {edit&&<form className="rename-session" onSubmit={event=>{event.preventDefault();void run(async()=>{await mutate(edit.session,{...edit.metadata,title:title||null});setEdit(undefined)})}}><label>Conversation title<input aria-label="Conversation title" value={title} onChange={event=>setTitle(event.target.value)} maxLength={256}/></label><Button type="submit" size="sm">Save title</Button><Button size="sm" onClick={()=>setEdit(undefined)}>Cancel</Button></form>}
    {external&&<details className="external-navigation"><summary>External agents</summary><section aria-label="External agents"><Button size="sm" aria-label="Refresh external conversations" onClick={()=>void run(()=>input.command({action:'external_catalog',next:false}))}>Refresh</Button>
      {external.endpoints.filter(endpoint=>endpoint.enabled).map(endpoint=><Button key={endpoint.id} size="sm" onClick={()=>void run(()=>startExternal(endpoint.id))}>{pendingStarts[endpoint.id]?'Check start':'Start'} {endpoint.id}</Button>)}
      {!external.endpoints.some(endpoint=>endpoint.enabled)&&<p className="hint">No external agents enabled in this Host Profile.</p>}
      {external.conversations.map(item=><button className="nav-item" key={item.id} title={item.id} onClick={()=>void run(()=>input.open({action:'external_open',id:item.id}))}><strong>{item.endpoint}</strong><small>{basename(item.cwd)}</small></button>)}
      {external.more&&<Button size="sm" onClick={()=>void run(()=>input.command({action:'external_catalog',next:true}))}>More external conversations</Button>}
    </section></details>}
  </>
}
