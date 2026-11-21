import {createContext,useCallback,useContext,useEffect,useLayoutEffect,useMemo,useRef,useState,type ReactNode} from 'react'
import {bindSnapshotSelector} from '../vendor/dsh/renderer/bind.ts'
import {defaultLayout,validateLayout,PresentationStore} from '../presentation-store.js'
import {observable,useView,type View} from './bridge.ts'
import {applyIntent} from '../device-store.js'
import {LayoutEdits} from '../layout-edits.js'

export interface Layout {version:6;conversationWidth:number|null;resourcesClosed:boolean;detail:'compact'|'standard'|'detailed'|'verbose';workspaces:{id:string;expanded:boolean}[]}
export const LayoutContext=createContext<{layout:Layout;update:(patch:Partial<Layout>)=>void;expand:(id:string,expanded?:boolean)=>void}>({layout:defaultLayout as Layout,update:()=>{},expand:()=>{}})
export const useLayout=()=>useContext(LayoutContext)
export const presentationIdentity = observable<string | undefined>(undefined)
export function hasOpenOverlay(except?: Element | null) {
  for (const element of document.querySelectorAll('dialog[open],[aria-modal="true"],[role="menu"]')) {
    if (element !== except && !element.closest('details:not([open]),dialog:not([open]),[hidden],[inert]') && element.getClientRects().length) return true
  }
  return false
}
const useIdentity = bindSnapshotSelector(presentationIdentity)
type Theme = NonNullable<View['appearance']>['theme']
const themePreview = observable<{identity: string | undefined; theme: Theme} | undefined>(undefined)
const useThemePreview = bindSnapshotSelector(themePreview)
export async function previewThemeWrite(theme: Theme, write: () => Promise<void>) {
  const preview = {identity: presentationIdentity.getSnapshot(), theme}
  themePreview.set(preview)
  try { await write() }
  finally { if (themePreview.getSnapshot() === preview) themePreview.set(undefined) }
}
export function usePresentation() {
  const identity = useIdentity(value=>value)
  const appearance = useView(view=>view?.appearance)
  const [layout,setLayout] = useState<Layout>(validateLayout(defaultLayout) as Layout)
  const [storageNotice,setStorageNotice] = useState('')
  const [storageRecovery,setStorageRecovery] = useState<'retry'|'reset'|'reload'>('retry')
  const [attempt,setAttempt] = useState(0)
  const storage = useRef<PresentationStore>()
  const revision = useRef(0)
  const owner = useRef(identity)
  const pending = useRef(new LayoutEdits())
  const save = useRef<()=>void>(()=>{})
  const preview = useThemePreview(value=>value?.identity === identity ? value?.theme : undefined)
  const theme = preview ?? appearance?.theme ?? 'system'
  const fontSize = appearance?.content_font_size ?? 14
  const [themeColor] = useState(()=>{
    const meta = document.createElement('meta')
    meta.name = 'theme-color'
    return meta
  })
  useLayoutEffect(()=>{
    document.head.append(themeColor)
    return ()=>themeColor.remove()
  },[themeColor])
  useLayoutEffect(()=>{
    const root = document.documentElement
    root.dataset.theme = theme
    const synchronize = ()=>{ themeColor.content = getComputedStyle(root).backgroundColor }
    synchronize()
    const system = theme === 'system' ? matchMedia('(prefers-color-scheme: dark)') : undefined
    system?.addEventListener('change',synchronize)
    return ()=>{
      system?.removeEventListener('change',synchronize)
    }
  },[theme,themeColor])
  useLayoutEffect(()=>{
    document.documentElement.dataset.contentSize = String(fontSize)
  },[fontSize])
  useEffect(()=>{
    const openingRevision=++revision.current
    let active = true
    let unsubscribe:(()=>void)|undefined
    let focus:(()=>void)|undefined
    setStorageNotice(''); storage.current = undefined
    if(owner.current!==identity){owner.current=identity;pending.current=new LayoutEdits();setLayout(validateLayout(defaultLayout) as Layout)}
    if (!identity) return
    let store: PresentationStore | undefined
    void (async()=>{
      try {
        store = await PresentationStore.open(identity, {onVersionChange:()=>{
          if(!active)return
          storage.current=undefined
          setStorageRecovery('reload')
          setStorageNotice('Layout storage was updated in another window. Reload this window.')
        }}) as PresentationStore
        if (!active) {store.close();return}
        storage.current = store
        let reading = false,readAgain=false
        const reload = () => {
          if(!active || !store || pending.current.size)return
          if(reading){readAgain=true;return}
          reading=true;readAgain=false
          const loadingRevision=revision.current
          void store.load().then(
            value=>{if(active&&!readAgain&&!pending.current.size&&revision.current===loadingRevision){setLayout(validateLayout(value));setStorageNotice('')}},
            ()=>{if(active&&!readAgain&&!pending.current.size&&revision.current===loadingRevision){setStorageRecovery('reset');setStorageNotice('Saved layout could not be loaded. Reset layout to repair it.')}},
          ).finally(()=>{reading=false;if(readAgain)reload()})
        }
        unsubscribe=store.subscribe(reload);focus=reload;window.addEventListener('focus',reload)
        let saving=false
        const flush=async()=>{
          if(saving || !active || !store)return
          saving=true
          try {
            while(active && pending.current.size){
              const [key,intent]=pending.current.first()!
              await store.apply(intent)
              if(active)pending.current.saved(key,intent)
            }
            if(active){setStorageNotice('');reload()}
          } catch {
            if(active){setStorageRecovery('retry');setStorageNotice('Layout changes could not be saved. Retry layout to save them.')}
          } finally {saving=false}
        }
        save.current=()=>void flush()
        if(pending.current.size)save.current()
        else if(revision.current===openingRevision)reload()
      } catch {
        store?.close()
        if(active){setStorageRecovery('retry');setStorageNotice('Layout storage could not be opened. Close older windows, then retry.')}
      }
    })()
    return ()=>{active=false;unsubscribe?.();if(focus)window.removeEventListener('focus',focus);store?.close();storage.current=undefined;save.current=()=>{}}
  },[identity,attempt])
  const apply = useCallback((intent:Record<string,unknown>) => {
    ++revision.current
    setLayout(current=>intent.kind==='reset'?validateLayout(defaultLayout):applyIntent('layouts',current,intent))
    pending.current.add(intent)
    if(!storage.current){setStorageRecovery('retry');setStorageNotice('Layout changes are not saved on this device.');return}
    save.current()
  },[])
  const update=useCallback((patch:Partial<Layout>)=>apply({kind:'patch',patch}),[apply])
  const expand=useCallback((id:string,expanded?:boolean)=>{
    if(expanded!==undefined)apply({kind:'workspace',id,expanded})
    else if(storage.current)apply({kind:'reveal_workspace',id})
  },[apply])
  return {layout,storageNotice,storageRecovery,retry:()=>setAttempt(value=>value+1),reset:()=>apply({kind:'reset'}),update,expand}
}
// Long-lived resource listeners must route through the current viewport, even
// when they retained the callback before a responsive transition.
export function useResourceVisibility(desktop:Layout,persist:(patch:Partial<Layout>)=>void,narrow:boolean,available=true) {
  const [closed,setClosed]=useState(true)
  const current=useRef({persist,narrow});current.current={persist,narrow}
  const update=useCallback((patch:Partial<Layout>)=>{
    const {persist,narrow}=current.current
    if(!narrow){persist(patch);return}
    const {resourcesClosed,...saved}=patch
    if(resourcesClosed!==undefined)setClosed(resourcesClosed)
    if(Object.keys(saved).length)persist(saved)
  },[])
  const layout=useMemo(()=>!available?{...desktop,resourcesClosed:true}:narrow?{...desktop,resourcesClosed:closed}:desktop,[available,narrow,desktop,closed])
  return {layout,update}
}
export interface Viewport {width:number;height:number}
export function useViewport():Viewport {
  const [viewport,setViewport]=useState(()=>({width:innerWidth,height:innerHeight}))
  useEffect(()=>{
    let frame=0
    const resize=()=>{if(!frame)frame=requestAnimationFrame(()=>{
      frame=0
      const width=innerWidth,height=innerHeight
      setViewport(previous=>previous.width===width&&previous.height===height?previous:{width,height})
    })}
    window.addEventListener('resize',resize)
    return ()=>{window.removeEventListener('resize',resize);cancelAnimationFrame(frame)}
  },[])
  return viewport
}
export function Modal({children,close,className,label,initialFocus,open,lightDismiss=false,fallbackFocus}:{children:ReactNode;close:()=>void;className:string;label:string;initialFocus?:string;open?:boolean;lightDismiss?:boolean;fallbackFocus?:string}) {
  const ref = useRef<HTMLDialogElement>(null)
  const [mountTrigger] = useState(()=>open === undefined && document.activeElement instanceof HTMLElement ? document.activeElement : undefined)
  const onClose = useRef(close); onClose.current=close
  useEffect(()=>{
    if (open === false) return
    const trigger = mountTrigger ?? document.activeElement
    const dialog=ref.current!;dialog.showModal()
    if(initialFocus)dialog.querySelector<HTMLElement>(initialFocus)?.focus()
    return ()=>{
      dialog.close()
      const target = trigger instanceof HTMLElement && trigger.getClientRects().length && !trigger.matches(':disabled') && !dialog.contains(trigger)
        ? trigger : document.querySelector<HTMLElement>(fallbackFocus ?? '.pane.selected textarea:not([disabled])')
      if (target) target.focus()
    }
  },[open,mountTrigger,initialFocus,fallbackFocus])
  return <dialog ref={ref} className={className} aria-label={label} {...(lightDismiss ? {closedby:'any'} : {})}
    onCancel={event=>{event.preventDefault();event.stopPropagation();onClose.current()}}
    onKeyDown={event=>{
      if(event.key!=='Tab'||event.defaultPrevented||event.ctrlKey||event.metaKey||event.altKey||event.nativeEvent.isComposing)return
      const dialog=event.currentTarget,active=dialog.ownerDocument.activeElement
      if(!active||active.closest('dialog,[role="dialog"],[role="menu"]')!==dialog)return
      const controls=Array.from(dialog.querySelectorAll<HTMLElement>('button,input,textarea,select,a[href],summary,[tabindex],[contenteditable="true"]'))
        .filter(el=>el.closest('dialog')===dialog&&!el.closest('[inert]')&&!el.matches(':disabled,[tabindex^="-"]')&&el.checkVisibility({visibilityProperty:true}))
      const first=controls[0]??dialog,last=controls.at(-1)??dialog
      if(active===dialog||active===(event.shiftKey?first:last)){
        event.preventDefault()
        const destination=event.shiftKey?last:first
        destination.focus()
      }
    }}>{children}</dialog>
}
export function ResizeHandle({name,value,min,max,onChange,reverse=false}:{name:string;value:number;min:number;max:number;onChange:(value:number)=>void;reverse?:boolean}) {
  const start=useRef<{pointer:number;x:number;width:number;latest:number;root:HTMLElement;previous:string;frame?:number}>()
  const property=`--${name==='navigation'?'navigation':'resources'}-width`
  const bounded=(width:number)=>Math.max(min,Math.min(max,Math.round(width)))
  const finish=(pointer:number,end?:number)=>{
    const drag=start.current
    if(!drag || pointer!==drag.pointer)return
    start.current=undefined
    if(drag.frame!==undefined)cancelAnimationFrame(drag.frame)
    drag.root.style.setProperty(property,drag.previous)
    if(end!==undefined && end!==drag.x){
      const width=bounded(drag.width+(end-drag.x)*(reverse?-1:1))
      if(width!==drag.width)onChange(width)
    }
  }
  useLayoutEffect(()=>()=>{if(start.current)finish(start.current.pointer)},[property])
  return <div className="panel-resize" role="separator" tabIndex={0} aria-label={`Resize ${name}`} aria-orientation="vertical" aria-valuemin={min} aria-valuemax={max} aria-valuenow={value}
    onKeyDown={event=>{
      if(event.key==='Escape' && start.current){finish(start.current.pointer);event.preventDefault();return}
      const delta=event.key==='ArrowLeft'?-10:event.key==='ArrowRight'?10:0
      if(delta){
        event.preventDefault()
        const visible=event.currentTarget.parentElement!.getBoundingClientRect().width
        const width=bounded(visible+delta*(reverse?-1:1))
        if(width!==visible)onChange(width)
      }
    }}
    onPointerDown={event=>{
      if(event.button!==0 || start.current)return
      const root=event.currentTarget.closest<HTMLElement>('#workbench')
      if(!root)return
      const width=event.currentTarget.parentElement!.getBoundingClientRect().width
      start.current={pointer:event.pointerId,x:event.clientX,width,latest:width,root,previous:root.style.getPropertyValue(property)}
      event.currentTarget.setPointerCapture(event.pointerId)
    }}
    onPointerMove={event=>{
      const drag=start.current
      if(!drag || event.pointerId!==drag.pointer)return
      drag.latest=bounded(drag.width+(event.clientX-drag.x)*(reverse?-1:1))
      drag.frame??=requestAnimationFrame(()=>{
        drag.frame=undefined
        drag.root.style.setProperty(property,`${drag.latest}px`)
      })
    }}
    onPointerUp={event=>finish(event.pointerId,event.clientX)}
    onPointerCancel={event=>finish(event.pointerId)} onLostPointerCapture={event=>finish(event.pointerId)}/>

}
export interface PaletteAction {label:string;run:()=>void}
export function CommandPalette({actions,close}:{actions:PaletteAction[];close:()=>void}) {
  const [query,setQuery]=useState(''),[selected,setSelected]=useState(0)
  const visible=actions.filter(action=>action.label.toLowerCase().includes(query.toLowerCase()))
  const invoke=(action:PaletteAction)=>{close();setTimeout(action.run,0)}
  return <Modal className="command-palette" label="Command palette" close={close} initialFocus='[aria-label="Search commands"]'><div className="dialog-heading"><h2>Commands</h2><button onClick={close} aria-label="Close commands">×</button></div>
    <input aria-label="Search commands" role="combobox" aria-controls="palette-results" aria-expanded="true" aria-activedescendant={visible.length?`palette-${selected}`:undefined} value={query} placeholder="Search actions…" onChange={event=>{setQuery(event.target.value);setSelected(0)}} onKeyDown={event=>{
      if(event.key==='ArrowDown'||event.key==='ArrowUp'){event.preventDefault();setSelected(current=>Math.max(0,Math.min(visible.length-1,current+(event.key==='ArrowDown'?1:-1))))}
      if(event.key==='Enter'&&visible[selected]){event.preventDefault();invoke(visible[selected])}
    }}/>
    <div id="palette-results" role="listbox">{visible.map((action,index)=><button key={action.label} id={`palette-${index}`} role="option" aria-selected={index===selected} onClick={()=>invoke(action)}>{action.label}</button>)}{!visible.length&&<p className="hint">No matching actions</p>}</div>
  </Modal>
}
