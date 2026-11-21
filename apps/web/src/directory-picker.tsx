import css from "./directory-picker.module.css"
import {useEffect,useLayoutEffect,useMemo,useRef,useState} from 'react'
import {input,useView,type ExecutionLocation} from './bridge.ts'
import {Modal} from './presentation.tsx'
import {Button} from './button.tsx'
import {directory} from './directory-input.ts';
import {IconChevronRightOutlineRegular,IconFolderCloseRegular,IconFolderOpenRegular,IconPlusOutlineRegular} from '../vendor/dsh/dockkit-primitives/icons/index.tsx'
interface Entry {name:string;path:string;hidden:boolean;symlink:boolean}
interface Listing {path:string;home:string|null;breadcrumbs:string[];entries:Entry[];truncated:boolean;unrepresentable:boolean}
function directoryCrumbs(listing?:Listing) {
  if(!listing)return []
  let path=listing.path.slice(0,listing.path.startsWith('/')?1:3)
  const crumbs=[{label:path===listing.home?'Home':path,path}]
  for(const label of listing.breadcrumbs){
    path=path.endsWith('/')?path+label:path+'/'+label
    if(path===listing.home)crumbs.length=0
    crumbs.push({label:path===listing.home?'Home':label,path})
  }
  return crumbs
}
export function chooseWorkspace() {window.dispatchEvent(new Event('rsi:choose-workspace'))}
export function DirectoryPickerHost() {
  const [open,setOpen]=useState(false)
  useEffect(()=>{const show=()=>setOpen(true);window.addEventListener('rsi:choose-workspace',show);return()=>window.removeEventListener('rsi:choose-workspace',show)},[])
  return open?<DirectoryPicker close={()=>setOpen(false)}/>:null
}
function DirectoryPicker({close}:{close:()=>void}) {
  const [selected,setSelected]=useState('local')
  const ssh=useView(view=>view?.plugins?.ssh)
  const targets=ssh?.available ? ssh.catalog?.targets.filter(target=>target.permissions.use_target) : undefined
  const location:ExecutionLocation=selected==='local'?{kind:'local'}:{kind:'ssh',target:selected}
  const [columns,setColumns]=useState<Listing[]>([]),[path,setPath]=useState(''),[editing,setEditing]=useState(false)
  const [hidden,setHidden]=useState(false),[available,setAvailable]=useState(false),[status,setStatus]=useState('Checking location…')
  const [busy,setBusy]=useState<string>(),[name,setName]=useState<string|null>(null),[uncertain,setUncertain]=useState(false)
  const readId=useRef<string>(),request=useRef(0),current=columns.at(-1)
  const selectedEntry=useRef<HTMLButtonElement>(null),header=useRef<HTMLElement>(null)
  const focusAfter=useRef<'row'|'path'>()
  useLayoutEffect(()=>{
    if(editing||busy)return
    if(focusAfter.current==='row')selectedEntry.current?.focus()
    if(focusAfter.current==='path'&&(document.activeElement===document.body||document.activeElement===header.current?.closest('dialog')))
      header.current?.querySelector<HTMLButtonElement>('.directory-location > button')?.focus()
    focusAfter.current=undefined
  },[columns,editing,busy])
  function cancelRead() {
    request.current++
    const id=readId.current
    if(id){readId.current=undefined;void input.directory({action:'cancel',request_id:id}).catch(()=>{})}
  }
  async function read(path:string|null) {
    const id=crypto.randomUUID()
    readId.current=id
    try {return await directory<Listing>({action:'list',location,path,request_id:id})}
    finally {if(readId.current===id)readId.current=undefined}
  }
  async function browse(path:string|null,parent?:Listing) {
    const revision=++request.current;setBusy('list');setStatus('')
    try {
      const listing=await read(path)
      if(revision!==request.current)return
      focusAfter.current=parent?'row':editing?'path':undefined
      setColumns(parent&&parent.entries.some(entry=>entry.path===listing.path)?[parent,listing]:[listing]);setPath(listing.path);setEditing(false);setUncertain(false)
      return listing
    }catch(error){if(revision===request.current)setStatus(error instanceof Error?error.message:String(error))}
    finally{if(revision===request.current)setBusy(undefined)}
  }
  useEffect(()=>{
    const revision=++request.current
    setColumns([]);setPath('');setName(null);setUncertain(false);setAvailable(false);setBusy('check');setStatus('Checking location…')
    void (async()=>{
      try {
        const state=await directory<{supported:boolean;allowed:boolean}>({action:'status',location})
        if(revision!==request.current)return
        setAvailable(state.supported&&state.allowed)
        if(state.supported&&state.allowed)await browse(null)
        else {setBusy(undefined);setEditing(true);setStatus(state.supported?(location.kind==='local'?'Local browsing requires a configuration grant. Enter a known path to register it.':'This SSH target requires Use permission.'):'Directory browsing is unavailable for this location.')}
      }catch(error){if(revision===request.current){setBusy(undefined);setEditing(true);setStatus(error instanceof Error?error.message:String(error))}}
    })()
    return cancelRead
  },[selected])
  useEffect(()=>{
    if(ssh?.available)void input.command({action:'plugins',command:{kind:'ssh',command:{kind:'read'}}}).catch(()=>{})
  },[ssh?.available])
  async function create() {
    if(!current||!name?.trim()||busy||uncertain)return
    const revision=request.current
    setBusy('create');setStatus('')
    try {
      const created=await directory<{path:string}>({action:'create',location,parent:current.path,name})
      if(revision!==request.current)return
      setName(null);const parent=await browse(current.path);if(parent)await browse(created.path,parent)
    }catch(error){
      if(revision!==request.current)return
      setBusy(undefined)
      setStatus(error instanceof Error?error.message:String(error))
      const kind=(error as {kind?:string})?.kind
      if(!kind||kind==='outcome_unknown'){setUncertain(true);setName(null)}
    }
  }
  async function open() {
    if(!path||busy)return
    const revision=request.current
    setBusy('open');setStatus('')
    try {
      let physical=path
      if(available && (editing||!current||current.path!==path)){
        const listing=await read(path)
        if(revision!==request.current)return
        physical=listing.path
      }
      await input.command({action:'register_workspace',location,path:physical})
      if(revision===request.current)close()
    }
    catch(error){if(revision===request.current){setStatus(error instanceof Error?error.message:String(error));setBusy(undefined)}}
  }
  const closing=()=>{if(busy!=='create'&&busy!=='open')close()}
  function dismiss() {
    if(!editing||!available){closing();return}
    if(busy==='open')return
    if(header.current?.contains(document.activeElement))focusAfter.current='path'
    cancelRead();setBusy(undefined);setPath(current?.path??'');setEditing(false);setStatus('')
  }
  function closeFolder() {
    if(!busy){setName(null);setStatus('')}
  }
  const crumbs=useMemo(()=>directoryCrumbs(current),[current])
  return <Modal label="Select workspace directory" className={`directory-picker ${css.scope}`} close={dismiss} initialFocus='[aria-label="Directory path"]'>
    <header ref={header}><h2>Select workspace directory</h2>
      {(!!targets?.length || selected!=='local') && <label>Execution location<select aria-label="Execution location" value={selected} disabled={!!busy} onChange={event=>setSelected(event.target.value)}><option value="local">Local Service</option>{selected!=='local'&&!targets?.some(target=>target.candidate.target===selected)&&<option value={selected}>Selected SSH target · unavailable</option>}{targets?.map(target=><option key={target.candidate.target} value={target.candidate.target}>{target.candidate.name}{target.connected?'':' · disconnected'}</option>)}</select></label>}
      {editing?<form className="directory-path" onSubmit={event=>{event.preventDefault();if(!busy&&path.trim()){if(available)void browse(path);else void open()}}}><input autoFocus aria-label="Directory path" value={path} disabled={busy==='open'} onChange={event=>{
        if(readId.current){cancelRead();setBusy(undefined)}
        setPath(event.target.value)
      }} maxLength={16384} placeholder="/path/to/project"/><Button type="submit" disabled={!!busy||!path.trim()}>{available?'Go':'Open'}</Button></form>:<div className="directory-location"><nav className="directory-breadcrumbs" aria-label="Directory breadcrumbs">
        {crumbs.map(crumb=><button key={crumb.path} title={crumb.path} disabled={!!busy} onClick={()=>void browse(crumb.path)}>{crumb.label}</button>)}
      </nav><Button aria-label="Edit directory path" disabled={!!busy} onClick={()=>setEditing(true)}>✎</Button></div>}
    </header>
    <div className="directory-columns" aria-busy={!!busy}>{columns.map((column,index)=>{
      const selectedPath=columns[index+1]?.path
      const entries=column.entries.filter(entry=>hidden||!entry.hidden||entry.path===selectedPath)
      return <section key={column.path} className="directory-column" aria-label={column.path}>
      {entries.map(entry=>{
        const active=selectedPath===entry.path, Folder=active?IconFolderOpenRegular:IconFolderCloseRegular
        return <button key={entry.name} ref={active?selectedEntry:undefined} className="directory-entry" aria-current={active||undefined} disabled={!!busy} title={entry.symlink?`Link to ${entry.path}`:entry.path} onClick={()=>void browse(entry.path,column)}>
          <Folder size={16}/><span>{entry.name}</span><IconChevronRightOutlineRegular size={12}/>
        </button>
      })}
      {entries.length===0&&<p className="hint">No visible folders.</p>}
      {column.truncated&&<p className="hint">This directory is truncated. Enter a deeper path directly.</p>}
      {column.unrepresentable&&<p className="hint">Some directory names cannot be displayed as UTF-8.</p>}
    </section>})}</div>
    {status&&name===null&&<p className="directory-status" role="status">{status}</p>}
    {uncertain&&current&&<Button onClick={()=>void browse(current.path)} disabled={!!busy}>Read parent to check result</Button>}
    <footer>{available&&<><Button variant="outline" icon={<IconPlusOutlineRegular size={14}/>} disabled={!!busy||!current||uncertain||editing&&path!==current.path} onClick={()=>{setName('');setStatus('')}}>New folder</Button><label className="check"><input type="checkbox" checked={hidden} onChange={event=>setHidden(event.target.checked)}/>Show hidden files</label></>}
      <span className="directory-spacer"/><Button variant="outline" onClick={closing} disabled={busy==='create'||busy==='open'}>Cancel</Button><Button variant="primary" disabled={!!busy||!path} onClick={()=>void open()}>Open</Button>
    </footer>
    {name!==null&&current&&<Modal label="New folder" className={css.createDialog} initialFocus="input" close={closeFolder}>
      <form className="directory-create" onSubmit={event=>{event.preventDefault();void create()}}>
        <h3>New folder</h3><p>New folder in "{current.breadcrumbs.at(-1)??current.path}"</p>
        <input aria-label="Folder name" placeholder="Untitled folder" value={name} disabled={!!busy} onChange={event=>setName(event.target.value)} maxLength={255}/>
        {status&&<p role="alert">{status}</p>}
        <div className={css.createActions}>
          <Button variant="outline" disabled={!!busy} onClick={closeFolder}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={!!busy||!name.trim()}>Create</Button>
        </div>
      </form>
    </Modal>}
  </Modal>
}
