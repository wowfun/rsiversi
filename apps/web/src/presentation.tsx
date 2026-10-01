import {createContext,useCallback,useContext,useEffect,useLayoutEffect,useRef,useState,type ReactNode} from 'react'
import {bindSnapshotSelector} from '../vendor/dsh/renderer/bind.ts'
import {defaultLayout,validateLayout,PresentationStore} from '../presentation-store.js'
import {observable,useView} from './bridge.ts'
import {applyIntent} from '../device-store.js'

export interface Layout {version:3;navigationWidth:number;resourcesWidth:number;navigation:'expanded'|'rail'|'hidden';resourcesClosed:boolean;detail:'compact'|'standard'|'detailed'|'verbose';workspaces:{id:string;expanded:boolean}[]}
export const LayoutContext=createContext<{layout:Layout;update:(patch:Partial<Layout>)=>void;expand:(id:string,expanded:boolean)=>void}>({layout:defaultLayout as Layout,update:()=>{},expand:()=>{}})
export const useLayout=()=>useContext(LayoutContext)
export const presentationIdentity = observable<string | undefined>(undefined)
const useIdentity = bindSnapshotSelector(presentationIdentity)
export function usePresentation() {
  const identity = useIdentity(value=>value)
  const appearance = useView(view=>view?.appearance)
  const [layout,setLayout] = useState<Layout>(validateLayout(defaultLayout) as Layout)
  const [storageNotice,setStorageNotice] = useState('')
  const storage = useRef<PresentationStore>()
  useLayoutEffect(()=>{
    document.documentElement.dataset.theme = appearance?.theme ?? 'system'
    document.documentElement.dataset.contentSize = String(appearance?.content_font_size ?? 14)
  },[appearance])
  useEffect(()=>{
    let active = true
    let unsubscribe:(()=>void)|undefined
    let focus:(()=>void)|undefined
    setStorageNotice(''); setLayout(validateLayout(defaultLayout) as Layout); storage.current = undefined
    if (!identity) return
    void (async()=>{
      let store: PresentationStore | undefined
      try {
        store = await PresentationStore.open(identity) as PresentationStore
        if (!active) {store.close();return}
        const value = await store.load()
        if (!active) {store.close();return}
        storage.current = store; setLayout(validateLayout(value))
        let reading = false
        const reload = () => {
          if(reading || !active || !store)return
          reading=true
          void store.load().then(value=>{if(active)setLayout(validateLayout(value))},()=>{if(active)setStorageNotice('Saved layout could not be refreshed.')}).finally(()=>{reading=false})
        }
        unsubscribe=store.subscribe(reload);focus=reload;window.addEventListener('focus',reload)
      } catch {
        store?.close()
        if(active){setStorageNotice('Layout preferences could not be loaded. Changes will last only for this connection.')}
      }
    })()
    return ()=>{active=false;unsubscribe?.();if(focus)window.removeEventListener('focus',focus);storage.current?.close();storage.current=undefined}
  },[identity])
  const apply = (intent:Record<string,unknown>) => {
    setLayout(current=>applyIntent('layouts',current,intent))
    const store=storage.current
    if(!store){setStorageNotice('Layout changes are not saved on this device.');return}
    void store.apply(intent).then(
      value=>{if(storage.current===store){setLayout(value);setStorageNotice('')}},
      ()=>{if(storage.current===store)setStorageNotice('Layout changes could not be saved. Reload to restore the saved layout.')},
    )
  }
  return {layout,storageNotice,update:(patch:Partial<Layout>)=>apply({kind:'patch',patch}),expand:(id:string,expanded:boolean)=>apply({kind:'workspace',id,expanded})}
}
// Long-lived resource listeners must route through the current viewport, even
// when they retained the callback before a responsive transition.
export function useResourceVisibility(desktop:Layout,persist:(patch:Partial<Layout>)=>void,narrow:boolean) {
  const [closed,setClosed]=useState(true)
  const current=useRef({persist,narrow});current.current={persist,narrow}
  const update=useCallback((patch:Partial<Layout>)=>{
    const {persist,narrow}=current.current
    if(!narrow){persist(patch);return}
    const {resourcesClosed,...saved}=patch
    if(resourcesClosed!==undefined)setClosed(resourcesClosed)
    if(Object.keys(saved).length)persist(saved)
  },[])
  return {layout:narrow?{...desktop,resourcesClosed:closed}:desktop,update}
}
export function useViewport() {
  const [viewport,setViewport]=useState(()=>({width:innerWidth,height:innerHeight}))
  useEffect(()=>{
    let frame=0
    const resize=()=>{if(!frame)frame=requestAnimationFrame(()=>{frame=0;setViewport({width:innerWidth,height:innerHeight})})}
    window.addEventListener('resize',resize)
    return ()=>{window.removeEventListener('resize',resize);cancelAnimationFrame(frame)}
  },[])
  return viewport
}
export function useNarrow(queryText = '(max-width: 767px)') {
  const [narrow,setNarrow] = useState(()=>matchMedia(queryText).matches)
  useEffect(()=>{
    const query = matchMedia(queryText)
    const change = ()=>setNarrow(query.matches)
    query.addEventListener('change',change); return ()=>query.removeEventListener('change',change)
  },[queryText])
  return narrow
}
export function Modal({children,close,className,label}:{children:ReactNode;close:()=>void;className:string;label:string}) {
  const ref = useRef<HTMLDialogElement>(null)
  const onClose = useRef(close); onClose.current=close
  useEffect(()=>{
    const trigger = document.activeElement instanceof HTMLElement ? document.activeElement : undefined
    const dialog=ref.current!;dialog.showModal()
    return ()=>{
      dialog.close()
      if(trigger?.isConnected && trigger.getClientRects().length)trigger.focus()
      else document.querySelector<HTMLTextAreaElement>('.pane.selected textarea:not([disabled])')?.focus()
    }
  },[])
  return <dialog ref={ref} className={className} aria-label={label} onCancel={event=>{event.preventDefault();onClose.current()}}>{children}</dialog>
}
export function ResizeHandle({name,value,min,max,onChange,reverse=false}:{name:string;value:number;min:number;max:number;onChange:(value:number)=>void;reverse?:boolean}) {
  const start=useRef<{x:number;value:number;preview:number;root:HTMLElement;previous:string}>()
  const property=`--${name==='navigation'?'navigation':'resources'}-width`
  const bounded=(value:number)=>Math.max(min,Math.min(max,value))
  const finish=(commit:boolean)=>{const drag=start.current;if(!drag)return;start.current=undefined;drag.root.style.setProperty(property,drag.previous);if(commit)onChange(drag.preview)}
  return <div className="panel-resize" role="separator" tabIndex={0} aria-label={`Resize ${name}`} aria-orientation="vertical" aria-valuemin={min} aria-valuemax={max} aria-valuenow={value}
    onKeyDown={event=>{const delta=event.key==='ArrowLeft'?-10:event.key==='ArrowRight'?10:0;if(delta){event.preventDefault();onChange(bounded(value+delta*(reverse?-1:1)))}}}
    onPointerDown={event=>{const root=document.getElementById('workbench');if(!root)return;start.current={x:event.clientX,value,preview:value,root,previous:root.style.getPropertyValue(property)};event.currentTarget.setPointerCapture(event.pointerId)}}
    onPointerMove={event=>{const drag=start.current;if(drag){drag.preview=bounded(drag.value+(event.clientX-drag.x)*(reverse?-1:1));drag.root.style.setProperty(property,`${drag.preview}px`)}}}
    onPointerUp={()=>finish(true)} onPointerCancel={()=>finish(false)} onLostPointerCapture={()=>finish(false)}/>

}
export interface PaletteAction {label:string;run:()=>void}
export function CommandPalette({actions,close}:{actions:PaletteAction[];close:()=>void}) {
  const [query,setQuery]=useState(''),[selected,setSelected]=useState(0)
  const visible=actions.filter(action=>action.label.toLowerCase().includes(query.toLowerCase()))
  const invoke=(action:PaletteAction)=>{close();setTimeout(action.run,0)}
  return <Modal className="command-palette" label="Command palette" close={close}><div className="dialog-heading"><h2>Commands</h2><button onClick={close} aria-label="Close commands">×</button></div>
    <input autoFocus aria-label="Search commands" role="combobox" aria-controls="palette-results" aria-expanded="true" aria-activedescendant={visible.length?`palette-${selected}`:undefined} value={query} placeholder="Search actions…" onChange={event=>{setQuery(event.target.value);setSelected(0)}} onKeyDown={event=>{
      if(event.key==='ArrowDown'||event.key==='ArrowUp'){event.preventDefault();setSelected(current=>Math.max(0,Math.min(visible.length-1,current+(event.key==='ArrowDown'?1:-1))))}
      if(event.key==='Enter'&&visible[selected]){event.preventDefault();invoke(visible[selected])}
    }}/>
    <div id="palette-results" role="listbox">{visible.map((action,index)=><button key={action.label} id={`palette-${index}`} role="option" aria-selected={index===selected} onClick={()=>invoke(action)}>{action.label}</button>)}{!visible.length&&<p className="hint">No matching actions</p>}</div>
  </Modal>
}
