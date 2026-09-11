import license from '../vendor/dsh/LICENSE?raw'
import type { PropsRenderSlots } from '@rsi/dsh-slots'
import { useState } from 'react'
import { createRoot } from 'react-dom/client'
import { flushSync } from 'react-dom'
import { Button } from '../vendor/dsh/primitives/Button.tsx'
import { StateDot } from '../vendor/dsh/primitives/StateDot.tsx'
import { slots, host, renderer } from './slots.tsx'
import { input, run, useView, useSelected } from './bridge.ts'
import { Navigation } from './navigation.tsx'
import { Setup } from './setup.tsx'
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
  return <>
    <header className="app-header"><a className="wordmark" href="/" aria-label="RSI home">rsi<span className="wordmark-dot">.</span></a><span className="app-purpose">Workspace</span><span id="connection-state" className="connection-state" role="status">Disconnected</span><button id="sign-out" className="quiet" hidden>Sign out</button></header>
    <div id="notice" className="notice" role="alert" hidden/>
    <main id="login" className="login"><div className="login-heading"><span className="eyebrow">Connect your service</span><h1>Open your workspace.</h1><p>Use a device receipt to connect this browser to your RSI service.</p></div><form id="login-form" className="login-form"><label htmlFor="receipt">Device registration receipt</label><textarea id="receipt" rows={6} spellCheck={false} autoComplete="off" placeholder="Paste your device receipt"/><p className="hint">On the service computer, run <code>rsi --profile devices -- register browser</code>.</p><label id="dev-http-label" className="check" hidden><input id="dev-http" type="checkbox"/>Allow local HTTP for development</label><div className="actions"><button id="connect" className="primary" type="submit">Connect</button><button id="reconnect" type="button" hidden>Reconnect with this browser</button></div><p className="hint">The receipt is used once. Device tokens are not saved in browser storage.</p></form></main>
    <main id="workbench" className={`workbench${settings ? ' showing-settings' : ''}`} hidden>
      <aside className="sidebar" aria-label="Workspace navigation">{renderSlot('rsi.navigation',{})}<div className="sidebar-footer"><Button id="settings-open" size="sm" variant={settings?'toolbar':'ghost'} onClick={()=>setSettings(!settings)}>Settings</Button><Button size="sm" onClick={()=>setLicenses(true)}>Licenses</Button></div></aside>
      {renderSlot('rsi.main',{settings,closeSettings:()=>setSettings(false)})}
      <aside className="resources" aria-label="Session resources" hidden={settings}>{renderSlot('rsi.resources',{})}</aside>
    </main>
    {licenses && <dialog open className="license-dialog"><h2>Third-party software</h2><p>DeepSeek Harness · MIT · c291e7961a515f6d7af9304e7fd1d257929aef26</p><pre>{license}</pre><Button onClick={()=>setLicenses(false)}>Close licenses</Button></dialog>}
    <dialog id="detail" className="detail-dialog"><div className="dialog-heading"><h2 id="detail-title">Details</h2><button id="detail-close" aria-label="Close details">×</button></div><div id="detail-body"/></dialog>
  </>
}
function Main({settings,closeSettings}: {settings:boolean;closeSettings:()=>void}) {
  const surfaceKeys = useView(view=>Object.keys(view?.surfaces ?? {}).join(','))
  const setup = useView(view=>view?.setup), selected = useSelected(value=>value)
  const [mode,setMode] = useState('chat')
  return <section className={`workspace-main mode-${mode}`} aria-label="Conversations">
    <div hidden={settings} className="session-toolbar"><nav className="pane-tabs" aria-label="Conversation surfaces">{surfaceKeys.split(',').filter(Boolean).map(key=><span key={key}><Button size="sm" id={`pane-tab-${key}`} aria-pressed={selected===key} onClick={()=>input.select(key)}>{key==='main'?'Conversation':key==='compare'?'Compare':key}</Button>{key!=='main' && <Button size="sm" aria-label={`Close ${key}`} onClick={()=>void run(()=>input.close(key))}>×</Button>}</span>)}{surfaceKeys.split(',').filter(Boolean).length<2 && <Button size="sm" id="add-surface" onClick={()=>void run(()=>input.add('compare'))}>+ Compare</Button>}</nav><nav className="view-tabs" aria-label="Conversation view"><Button size="sm" aria-pressed={mode==='chat'} onClick={()=>setMode('chat')}>Chat</Button><Button size="sm" aria-pressed={mode==='trajectory'} onClick={()=>setMode('trajectory')}>Trajectory</Button></nav></div>
    {!setup?.agent?.default_model && <div className="setup-prompt" hidden={settings}><strong>Choose a model to start.</strong><span>Open Settings to configure a provider and your conversation defaults.</span></div>}
    <div id="panes" className="panes" hidden={settings}/>
    {settings && <Setup close={closeSettings}/>}
  </section>
}
function Resources() {
  const selected=useSelected(value=>value), surface=useView(view=>view?.surfaces[selected]), remote=useView(view=>view?.has_remote_ui)
  if (!surface) return <><h2>Session resources</h2><p className="hint">Open a conversation to see its files, activity and extensions.</p></>
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
