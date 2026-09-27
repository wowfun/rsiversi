import {useEffect,useRef,useState} from 'react'
import {input} from './bridge.ts'
import {Modal} from './presentation.tsx'
import {Button} from './button.tsx'
interface Entry {name:string;path:string;hidden:boolean;symlink:boolean}
interface Listing {path:string;home:string|null;breadcrumbs:string[];entries:Entry[];truncated:boolean;unrepresentable:boolean}
async function directory<T>(request:unknown):Promise<T> {
  const reply=JSON.parse(await input.directory(request))
  if(reply.status==='failed')throw Object.assign(new Error(reply.message),{kind:reply.failure.kind})
  return reply.value
}
export function chooseWorkspace() {window.dispatchEvent(new Event('rsi:choose-workspace'))}
export function DirectoryPickerHost() {
  const [open,setOpen]=useState(false)
  useEffect(()=>{const show=()=>setOpen(true);window.addEventListener('rsi:choose-workspace',show);return()=>window.removeEventListener('rsi:choose-workspace',show)},[])
  return open?<DirectoryPicker close={()=>setOpen(false)}/>:null
}
function DirectoryPicker({close}:{close:()=>void}) {
  const [columns,setColumns]=useState<Listing[]>([]),[path,setPath]=useState(''),[editing,setEditing]=useState(false)
  const [hidden,setHidden]=useState(false),[available,setAvailable]=useState(false),[status,setStatus]=useState('Checking Host…')
  const [busy,setBusy]=useState<string>(),[newFolder,setNewFolder]=useState(false),[name,setName]=useState(''),[uncertain,setUncertain]=useState(false)
  const readId=useRef<string>(),request=useRef(0),alive=useRef(true),current=columns.at(-1)
  async function browse(path:string|null,parent?:Listing) {
    const revision=++request.current,id=crypto.randomUUID();readId.current=id;setBusy('list');setStatus('')
    try {
      const listing=await directory<Listing>({action:'list',path,request_id:id})
      if(!alive.current||revision!==request.current)return
      setColumns(parent&&parent.entries.some(entry=>entry.path===listing.path)?[parent,listing]:[listing]);setPath(listing.path);setEditing(false);setUncertain(false)
      return listing
    }catch(error){if(alive.current&&revision===request.current)setStatus(String(error))}
    finally{if(alive.current&&revision===request.current)setBusy(undefined)}
  }
  useEffect(()=>{
    alive.current=true
    void (async()=>{
      try {
        const state=await directory<{supported:boolean;allowed:boolean}>({action:'status'})
        if(!alive.current)return
        setAvailable(state.supported&&state.allowed)
        if(state.supported&&state.allowed)await browse(null)
        else {setEditing(true);setStatus(state.supported?'Directory browsing requires a configuration grant. Enter a known path to register it.':'This Host supports manual workspace paths. Directory browsing is unavailable.')}
      }catch(error){if(alive.current){setEditing(true);setStatus(String(error))}}
    })()
    return()=>{alive.current=false;request.current++;if(readId.current)void input.directory({action:'cancel',request_id:readId.current}).catch(()=>{})}
  },[])
  async function create() {
    if(!current||busy||uncertain)return
    setBusy('create');setStatus('')
    try {
      const created=await directory<{path:string}>({action:'create',parent:current.path,name})
      if(!alive.current)return
      setNewFolder(false);setName('');const parent=await browse(current.path);if(parent)await browse(created.path,parent)
    }catch(error){if(alive.current){setStatus(String(error));const kind=(error as {kind?:string})?.kind;setUncertain(!kind||kind==='outcome_unknown')}}
    finally{if(alive.current)setBusy(undefined)}
  }
  async function open() {
    if(!path||busy)return
    setBusy('open');setStatus('')
    try {
      let physical=path
      if(available && (editing||!current||current.path!==path)){
        const id=crypto.randomUUID();readId.current=id
        const listing=await directory<Listing>({action:'list',path,request_id:id})
        physical=listing.path
      }
      await input.open({action:'register_workspace',path:physical});close()
    }
    catch(error){if(alive.current){setStatus(String(error));setBusy(undefined)}}
  }
  const closing=()=>{if(busy!=='create'&&busy!=='open')close()}
  const crumbs=current?[{label:'/',path:'/'},...current.breadcrumbs.map((label,index)=>({label,path:'/'+current.breadcrumbs.slice(0,index+1).join('/')}))]:[]
  return <Modal label="Select workspace directory" className="directory-picker" close={closing}>
    <header><h2>Select workspace directory</h2>
      {editing?<form className="directory-path" onSubmit={event=>{event.preventDefault();if(available)void browse(path);else void open()}}><input autoFocus aria-label="Directory path" value={path} onChange={event=>setPath(event.target.value)} maxLength={16384} placeholder="/path/to/project"/><Button type="submit" disabled={!!busy}>{available?'Go':'Open'}</Button></form>:<div className="directory-location"><nav className="directory-breadcrumbs" aria-label="Directory breadcrumbs">
        {current?.home&&<button disabled={!!busy} onClick={()=>void browse(current.home)}>Home</button>}
        {crumbs.map(crumb=><button key={crumb.path} title={crumb.path} disabled={!!busy} onClick={()=>void browse(crumb.path)}>{crumb.label}</button>)}
      </nav><Button aria-label="Edit directory path" disabled={!!busy} onClick={()=>setEditing(true)}>✎</Button></div>}
    </header>
    <div className="directory-columns" aria-busy={!!busy}>{columns.map((column,index)=><section key={column.path} className="directory-column" aria-label={column.path}>
      {column.entries.filter(entry=>hidden||!entry.hidden).map(entry=><button key={entry.name} className={`directory-entry${columns[index+1]?.path===entry.path?' selected':''}`} disabled={!!busy} title={entry.symlink?`Link to ${entry.path}`:entry.path} onClick={()=>void browse(entry.path,column)}><span aria-hidden="true">▱</span><span>{entry.name}</span><span aria-hidden="true">›</span></button>)}
      {!column.entries.some(entry=>hidden||!entry.hidden)&&<p className="hint">No visible folders.</p>}
      {column.truncated&&<p className="hint">This directory is truncated. Enter a deeper path directly.</p>}
      {column.unrepresentable&&<p className="hint">Some directory names cannot be displayed as UTF-8.</p>}
    </section>)}</div>
    {status&&<p className="directory-status" role="status">{status}</p>}
    {uncertain&&current&&<Button onClick={()=>void browse(current.path)} disabled={!!busy}>Read parent to check result</Button>}
    {newFolder&&<form className="directory-create" onSubmit={event=>{event.preventDefault();void create()}}><label>Folder name<input autoFocus value={name} onChange={event=>setName(event.target.value)} maxLength={255} required/></label><Button type="submit" disabled={!!busy||uncertain}>Create folder</Button><Button disabled={busy==='create'} onClick={()=>setNewFolder(false)}>Cancel new folder</Button></form>}
    <footer>{available&&<><Button variant="outline" disabled={!!busy||!current||uncertain} onClick={()=>setNewFolder(true)}>+ New folder</Button><label className="check"><input type="checkbox" checked={hidden} onChange={event=>setHidden(event.target.checked)}/>Show hidden files</label></>}
      <span className="directory-spacer"/><Button variant="outline" onClick={closing} disabled={busy==='create'||busy==='open'}>Cancel</Button><Button variant="primary" disabled={!!busy||!path} onClick={()=>void open()}>Open</Button>
    </footer>
  </Modal>
}
