import {createContext,useContext,useEffect,useLayoutEffect,useRef,useState,type ReactNode} from 'react'
import {bindSnapshotSelector} from '../vendor/dsh/renderer/bind.ts'
import {defaultLayout,validateLayout,PresentationStore} from '../presentation-store.js'
import {observable,useView} from './bridge.ts'

export interface Layout {version:2;navigationWidth:number;resourcesWidth:number;navigation:'expanded'|'rail'|'hidden';resourcesClosed:boolean;detail:'compact'|'standard'|'detailed'|'verbose';workspaces:{id:string;expanded:boolean}[]}
export const LayoutContext=createContext<{layout:Layout;update:(patch:Partial<Layout>)=>void}>({layout:defaultLayout as Layout,update:()=>{}})
export const useLayout=()=>useContext(LayoutContext)
export const presentationIdentity = observable<string | undefined>(undefined)
const useIdentity = bindSnapshotSelector(presentationIdentity)
export function usePresentation() {
  const identity = useIdentity(value=>value)
  const appearance = useView(view=>view?.appearance)
  const [layout,setLayout] = useState<Layout>(validateLayout(defaultLayout) as Layout)
  const [ready,setReady] = useState(false)
  const [storageNotice,setStorageNotice] = useState('')
  const storage = useRef<PresentationStore>()
  useLayoutEffect(()=>{
    document.documentElement.dataset.theme = appearance?.theme ?? 'system'
    document.documentElement.dataset.contentSize = String(appearance?.content_font_size ?? 14)
  },[appearance])
  useEffect(()=>{
    let active = true
    setReady(false); setStorageNotice(''); setLayout(validateLayout(defaultLayout) as Layout); storage.current = undefined
    if (!identity) return
    void (async()=>{
      let store: PresentationStore | undefined
      try {
        store = await PresentationStore.open(identity) as PresentationStore
        if (!active) {store.close();return}
        const value = await store.load()
        if (!active) {store.close();return}
        storage.current = store; setLayout(validateLayout(value)); setReady(true)
      } catch {
        store?.close()
        if(active){setReady(true);setStorageNotice('Layout preferences could not be loaded. Changes will last only for this connection.')}
      }
    })()
    return ()=>{active=false;storage.current?.close();storage.current=undefined}
  },[identity])
  useEffect(()=>{
    if (!ready || !storage.current) return
    const store = storage.current
    const timer = setTimeout(()=>{void store.save(layout).then(
      ()=>{if(storage.current===store)setStorageNotice('')},
      ()=>{if(storage.current===store)setStorageNotice('Layout preferences could not be saved. Current layout changes may be lost on reload.')},
    )},150)
    return ()=>clearTimeout(timer)
  },[layout,ready])
  return {layout,storageNotice,update:(patch:Partial<Layout>)=>setLayout(current=>validateLayout({...current,...patch}))}
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
  const start=useRef<{x:number;value:number}>()
  return <div className="panel-resize" role="separator" tabIndex={0} aria-label={`Resize ${name}`} aria-orientation="vertical" aria-valuemin={min} aria-valuemax={max} aria-valuenow={value}
    onKeyDown={event=>{const delta=event.key==='ArrowLeft'?-10:event.key==='ArrowRight'?10:0;if(delta){event.preventDefault();onChange(value+delta*(reverse?-1:1))}}}
    onPointerDown={event=>{start.current={x:event.clientX,value};event.currentTarget.setPointerCapture(event.pointerId)}}
    onPointerMove={event=>{if(start.current)onChange(start.current.value+(event.clientX-start.current.x)*(reverse?-1:1))}}
    onPointerUp={()=>{start.current=undefined}} onPointerCancel={()=>{start.current=undefined}}/>
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
