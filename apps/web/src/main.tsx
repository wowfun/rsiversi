import {DirectoryPickerHost,chooseWorkspace} from './directory-picker.tsx'
import license from '../vendor/dsh/LICENSE?raw'
import xtermLicense from '@xterm/xterm/LICENSE?raw'
import fitLicense from '@xterm/addon-fit/LICENSE?raw'
import type { PropsRenderSlots } from '@rsi/dsh-slots'
import { useState, useEffect, useLayoutEffect, useRef } from 'react'
import provenance from '../vendor/dsh/provenance.json'
import {usePresentation,useLayout,LayoutContext,useNarrow,ResizeHandle,Modal,CommandPalette} from './presentation.tsx'
import { createRoot } from 'react-dom/client'
import { flushSync } from 'react-dom'
import { Button } from './button.tsx'
import { StateDot } from '../vendor/dsh/primitives/StateDot.tsx'
import { slots, host, renderer } from './slots.tsx'
import { input, run, useView, useSelected } from './bridge.ts'
import { Navigation } from './navigation.tsx'
import { Setup } from './setup.tsx'
import {Terminals} from './terminal.tsx'
import './workbench.css'

declare module '@rsi/dsh-slots' {
  interface SlotMap {
    root: {kind:'single';scope:'root'}
    'rsi.navigation': {kind:'single';scope:'root'}
    'rsi.main': {kind:'single';scope:'root';owner:{settings:boolean;closeSettings:()=>void}}
    'rsi.resources': {kind:'single';scope:'session-maybe'}
  }
}
function Shell({renderSlot}: PropsRenderSlots<'rsi.navigation' | 'rsi.main' | 'rsi.resources'>) {
  const [settings,setSettings] = useState(false), [licenses,setLicenses] = useState(false)
  const {layout,update,storageNotice} = usePresentation(), narrow=useNarrow(), compact=useNarrow('(max-width: 1023px)')
  const navigationMode=narrow?'hidden':compact&&layout.navigation==='expanded'?'rail':layout.navigation
  const [drawer,setDrawer]=useState(false),[resourceDrawer,setResourceDrawer]=useState(false),[palette,setPalette]=useState(false)
  const selected=useSelected(value=>value)
  const currentWorkspace=useView(view=>view?.catalog.workspaces.find(workspace=>workspace.path===view?.surfaces[selected]?.path)?.id)
  const newConversation=()=>currentWorkspace?void run(()=>input.open({action:'create',workspace:currentWorkspace})):chooseWorkspace()
  const connected=useView(view=>!!view), preferenceError=useView(view=>view?.preference_error)
  const workbench=useRef<HTMLElement>(null)
  useLayoutEffect(()=>{workbench.current?.style.setProperty('--navigation-width',`${layout.navigationWidth}px`);workbench.current?.style.setProperty('--resources-width',`${layout.resourcesWidth}px`)},[layout])
  useEffect(()=>{const key=(event:KeyboardEvent)=>{if((event.ctrlKey||event.metaKey)&&event.key.toLowerCase()==='k'&&!event.isComposing&&connected){event.preventDefault();setPalette(value=>!value)}};window.addEventListener('keydown',key);return()=>window.removeEventListener('keydown',key)},[connected])
  useEffect(()=>{if(!connected){setPalette(false);setDrawer(false);setResourceDrawer(false);setSettings(false)}},[connected])
  const navigation=()=>narrow?setDrawer(value=>!value):update({navigation:layout.navigation==='expanded'?'rail':'expanded'})
  const resources=()=>narrow?setResourceDrawer(value=>!value):update({resourcesClosed:!layout.resourcesClosed})
  const appearance=()=>void run(()=>input.command({action:'settings_read',namespace:'rsi.client'}))
  const sidebar=<><div className="sidebar-brand"><a className="wordmark" href="/" aria-label="RSI home">rsi<span className="wordmark-dot">.</span></a><span>Workspace</span><Button size="sm" aria-label="Collapse navigation" onClick={navigation}>◧</Button></div><Button className="new-conversation" data-testid="new-conversation" variant="outline" onClick={newConversation}>⊕ New conversation</Button>{renderSlot('rsi.navigation',{})}<div className="sidebar-footer"><Button id="settings-open" size="sm" variant={settings?'toolbar':'ghost'} onClick={()=>{setSettings(!settings);setDrawer(false)}}>Settings</Button><Button size="sm" onClick={appearance}>Appearance</Button><Button size="sm" onClick={()=>setLicenses(true)}>Licenses</Button></div></>
  return <LayoutContext.Provider value={{layout,update}}>
    <DirectoryPickerHost/>
    <header className={`app-header${connected?' connected-header':''}`}><a className="wordmark" href="/" aria-label="RSI home">rsi<span className="wordmark-dot">.</span></a><span className="app-purpose">Workspace</span>{connected&&<div className="shell-actions"><Button size="sm" aria-label="Toggle navigation" aria-expanded={narrow?drawer:navigationMode==='expanded'} onClick={navigation}>☰</Button><Button size="sm" onClick={()=>setPalette(true)}>Commands</Button><Button size="sm" aria-label="Toggle resources" aria-expanded={narrow?resourceDrawer:!layout.resourcesClosed} onClick={resources}>Resources</Button></div>}<span id="connection-state" className="connection-state" role="status">Disconnected</span><button id="sign-out" className="quiet" hidden>Sign out</button></header>
    <div id="notice" className="notice" role="alert" hidden/>
    <main id="login" className="login"><div className="login-heading"><span className="eyebrow">Connect your service</span><h1>Open your workspace.</h1><p>Use a device receipt to connect this browser to your RSI service.</p></div><form id="login-form" className="login-form"><label htmlFor="receipt">Device registration receipt</label><textarea id="receipt" rows={6} spellCheck={false} autoComplete="off" placeholder="Paste your device receipt"/><p className="hint">On the service computer, run <code>rsi --profile devices -- register browser</code>.</p><label id="dev-http-label" className="check" hidden><input id="dev-http" type="checkbox"/>Allow local HTTP for development</label><div className="actions"><button id="connect" className="primary" type="submit">Connect</button><button id="reconnect" type="button" hidden>Reconnect with this browser</button></div><p className="hint">The receipt is used once. Device tokens are not saved in browser storage.</p></form></main>
    <div className="preference-error" role="alert" hidden={!preferenceError&&!storageNotice}>{[preferenceError,storageNotice].filter(Boolean).join(' ')}</div>
    <main ref={workbench} id="workbench" className={`workbench${settings ? ' showing-settings' : ''} navigation-${navigationMode}${layout.resourcesClosed?' resources-closed':''}`} hidden>
      {!narrow&&navigationMode==='expanded'&&<aside className="sidebar" aria-label="Workspace navigation">{sidebar}<ResizeHandle name="navigation" value={layout.navigationWidth} min={264} max={420} onChange={navigationWidth=>update({navigationWidth})}/></aside>}
      {!narrow&&navigationMode==='rail'&&<aside className="navigation-rail" aria-label="Workspace navigation"><span className="wordmark">rsi.</span><Button aria-label="Expand navigation" onClick={()=>setDrawer(true)}>◧</Button><Button aria-label="New conversation" onClick={chooseWorkspace}>+</Button><Button aria-label="Settings" onClick={()=>setSettings(true)}>⚙</Button></aside>}
      {renderSlot('rsi.main',{settings,closeSettings:()=>setSettings(false)})}
      {!narrow&&!layout.resourcesClosed&&!settings&&<aside className="resources" aria-label="Session resources"><ResizeHandle name="resources" value={layout.resourcesWidth} min={210} max={420} reverse onChange={resourcesWidth=>update({resourcesWidth})}/>{renderSlot('rsi.resources',{})}</aside>}
    </main>
    {connected&&drawer&&<Modal className="navigation-drawer" label="Workspace navigation" close={()=>setDrawer(false)}><button onClick={()=>setDrawer(false)}>Close navigation</button><div className="sidebar">{sidebar}</div></Modal>}
    {connected&&narrow&&resourceDrawer&&<Modal className="resource-drawer" label="Session resources" close={()=>setResourceDrawer(false)}><button onClick={()=>setResourceDrawer(false)}>Close resources</button>{renderSlot('rsi.resources',{})}</Modal>}
    {palette&&connected&&<CommandPalette close={()=>setPalette(false)} actions={[
      {label:'Focus conversation composer',run:()=>document.querySelector<HTMLTextAreaElement>('.pane.selected textarea:not([disabled])')?.focus()},
      {label:'Toggle navigation',run:navigation},{label:'Toggle resources',run:resources},
      {label:'Open Settings',run:()=>setSettings(true)},{label:'Appearance and input preferences',run:appearance},
      {label:'View licenses',run:()=>setLicenses(true)},
    ]}/>}
    {licenses && <Modal className="license-dialog" label="Third-party software" close={()=>setLicenses(false)}><h2>Third-party software</h2><p>DeepSeek Harness · MIT · {[...new Set(provenance.files.map(file=>file.revision))].map(revision=>revision.slice(0,7)).join(', ')}</p><pre>{license}</pre><p>xterm.js 6.0.0 · MIT</p><pre>{xtermLicense}</pre><p>xterm Fit 0.11.0 · MIT</p><pre>{fitLicense}</pre><Button onClick={()=>setLicenses(false)}>Close licenses</Button></Modal>}
    <dialog id="detail" className="detail-dialog"><div className="dialog-heading"><h2 id="detail-title">Details</h2><button id="detail-close" aria-label="Close details">×</button></div><div id="detail-body"/></dialog>
  </LayoutContext.Provider>
}
function Main({settings,closeSettings}: {settings:boolean;closeSettings:()=>void}) {
  const surfaceKeys = useView(view=>Object.keys(view?.surfaces ?? {}).join(','))
  const setup = useView(view=>view?.setup), selected = useSelected(value=>value)
  const {layout,update}=useLayout()
  const mode=layout.detail
  useLayoutEffect(()=>{document.documentElement.dataset.detail=mode;window.dispatchEvent(new Event('rsi:detail-mode'))},[mode])
  const [terminals,setTerminals] = useState(false)
  const surface=useView(view=>view?.surfaces[selected])
  return <section className={`workspace-main mode-${mode}`} aria-label="Conversations">
    <div hidden={settings} className="session-toolbar"><nav className="pane-tabs" aria-label="Conversation surfaces">{surfaceKeys.split(',').filter(Boolean).map(key=><span key={key}><Button size="sm" id={`pane-tab-${key}`} aria-pressed={selected===key} onClick={()=>input.select(key)}>{key==='main'?'Conversation':key==='compare'?'Compare':key}</Button>{key!=='main' && <Button size="sm" aria-label={`Close ${key}`} onClick={()=>void run(()=>input.close(key))}>×</Button>}</span>)}</nav><details className="conversation-menu"><summary aria-label="Conversation options">···</summary><div className="conversation-menu-items">{surfaceKeys.split(',').filter(Boolean).length>1&&<label className="mobile-surface">Conversation<select aria-label="Active conversation" value={selected} onChange={event=>input.select(event.target.value)}>{surfaceKeys.split(',').filter(Boolean).map(key=><option key={key} value={key}>{key==='main'?'Conversation':key==='compare'?'Compare':key}</option>)}</select></label>}<label>Detail level<select aria-label="Detail level" value={mode} onChange={event=>update({detail:event.target.value as typeof mode})}>{['compact','standard','detailed','verbose'].map(mode=><option key={mode} value={mode}>{mode[0].toUpperCase()+mode.slice(1)}</option>)}</select></label>{surfaceKeys.split(',').filter(Boolean).length<2&&<Button size="sm" id="add-surface" onClick={()=>void run(()=>input.add('compare'))}>Compare</Button>}<Button size="sm" disabled={surface?.kind==='external'} aria-pressed={terminals} onClick={()=>setTerminals(!terminals)}>Terminal</Button></div></details></div>
    {surface?.kind!=='external' && !setup?.agent?.default_model && <div className="setup-prompt" hidden={settings}><strong>Choose a model to start.</strong><span>Open Settings to configure a provider and your conversation defaults.</span></div>}
    <div id="panes" className="panes" hidden={settings}/>
    {terminals && !settings && surface && surface.kind!=='external' && <Terminals key={`${selected}:${surface.generation}`} pane={selected} surface={surface} hide={()=>setTerminals(false)}/>}
    {settings && <Setup close={closeSettings}/>}
  </section>
}
function Resources() {
  const selected=useSelected(value=>value), surface=useView(view=>view?.surfaces[selected]), remote=useView(view=>view?.has_remote_ui)
  if (!surface) return <><h2>Session resources</h2><p className="hint">Open a conversation to see its files, activity and extensions.</p></>
  if (surface.kind==='external' && surface.external) {
    const external=surface.external, snapshot=external.observed.snapshot
    return <><h2>External conversation</h2><p className="resource-path">{snapshot.cwd}</p><dl className="session-facts"><dt>Endpoint</dt><dd>{snapshot.endpoint}</dd><dt>Connection</dt><dd>{external.observed.connected?'Connected':'Disconnected'}</dd><dt>Observation</dt><dd>{snapshot.status}</dd><dt>Visible records</dt><dd>{external.blocks.length}</dd></dl><p className="hint">Switching views keeps the agent connected. Use Close peer to end its connection.</p></>
  }
  const invoke = (action:string,fields={})=>input.command({action,pane:selected,generation:surface.generation,...fields})
  return <><div className="section-heading"><h2>Session resources</h2><StateDot state={surface.transcript.status.toLowerCase()==='running'?'ongoing':'idle'}/></div><p className="resource-path" title={surface.path}>{surface.path}</p><div className="resource-buttons">{surface.ui_surfaces.map(item=><Button variant="outline" key={JSON.stringify(item.reference)} onClick={()=>void run(()=>invoke('ui_surface',{reference:item.reference}))}>{item.title}</Button>)}<Button onClick={()=>void run(()=>invoke('commands'))}>Session commands</Button>{remote && <Button onClick={()=>void run(()=>invoke('remote_ui_list'))}>Service extensions</Button>}</div><dl className="session-facts"><dt>Session</dt><dd>{surface.session}</dd><dt>Status</dt><dd>{surface.transcript.status || 'Ready'}</dd><dt>Loaded blocks</dt><dd>{surface.transcript.blocks.length}</dd></dl></>
}
const registrations = [
  slots.register({name:'root',registrant:'rsi.workbench',children:{'rsi.navigation':{kind:'single',scope:'root'},'rsi.main':{kind:'single',scope:'root'},'rsi.resources':{kind:'single',scope:'session-maybe'}}},Shell),
  slots.register({name:'rsi.navigation',registrant:'rsi.workbench.navigation'},Navigation),
  slots.register({name:'rsi.main',registrant:'rsi.gui'},Main),
  slots.register({name:'rsi.resources',registrant:'rsi.session.resources'},Resources),
]
void registrations
const root = createRoot(document.getElementById('root')!)
flushSync(()=>root.render(renderer.renderRoot(host,{})))
// Session DOM and independently admitted renderer graphs have their own awaited
// mount lifecycle inside this React-owned island. Bootstrap changes reload it.
const {initialize} = await import('../app.js')
initialize()
// Root composition changes request a full reload; feature components use React Refresh.
