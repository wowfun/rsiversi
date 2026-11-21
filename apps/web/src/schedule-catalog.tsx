import {useEffect,useId,useMemo,useRef,useState,useSyncExternalStore,type Ref} from 'react';
import {input,run,loadScheduleCatalog,scheduleCatalog,useView} from './bridge.ts';
import {navigationTitle} from '../conversation-title.js';
import {IconChevronRightOutlineRegular,IconClockOutlineRegular,IconCloseOutlineRegular,IconEllipsisOutlineRegular,IconSearchOutlineRegular} from '../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {frequency,NextRun,nextRunAt,ReminderDetails,reminderStatus,type Reminder} from './session/scheduled-tasks.tsx';
import {ScheduleRecommendations} from '../vendor/dsh/schedule/ScheduleRecommendations.tsx';
import {ScheduleEmptyIllustration} from '../vendor/dsh/schedule/ScheduleEmptyIllustration.tsx';
import {TextSearchIndex} from '../vendor/dsh/text-search.ts';
import {Modal} from './presentation.tsx';
import {Menu,MenuItemButton} from '../vendor/dsh/dockkit-primitives/Menu.tsx';
import {Button} from './button.tsx';
import dialogCss from './workspace-header.module.css';
import css from './schedule-catalog.module.css';

const filters = ['Active','Paused','Ended','All'] as const;
const creationPrompt = 'Please create an automation task based on the requirements.';

type CatalogTask = {session:string;row:Reminder;archived:boolean;executing:boolean;exhausted:boolean};
function TaskDetail({task,problem,id,now,closeRef,onClose,onDelete}: {
  task:CatalogTask;problem:boolean;id:string;now:number;closeRef:Ref<HTMLButtonElement>;onClose:()=>void;onDelete:()=>void;
}) {
  const [open,setOpen] = useState(false);
  const [busy,setBusy] = useState(false);
  const [error,setError] = useState('');
  const trigger = useRef<HTMLButtonElement>(null);
  const disabled = task.executing || problem || busy;
  const floor = task.row.last_delivery?.available_ms;
  const limited = floor !== undefined && floor > Date.now();
  async function start() {
    trigger.current?.focus();
    setOpen(false);
    setError('');
    setBusy(true);
    try { await input.command({action:'run_scheduled_task',task:{session_id:task.session,id:task.row.id}}); }
    catch (failure) { setError(failure instanceof Error ? failure.message : String(failure)); }
    finally { setBusy(false); }
  }
  return <aside id={id} className={css.detail} aria-label="Selected task" aria-busy={busy}><div className={css.detailActions}>
    <Menu open={open} onClose={()=>setOpen(false)} align="end" portal autoFocus
    anchor={<button ref={trigger} type="button" aria-label="Automation task actions" aria-haspopup="menu" aria-expanded={open} onClick={()=>setOpen(!open)}><IconEllipsisOutlineRegular size={16}/></button>}>
    <MenuItemButton disabled={disabled || task.archived || limited}
      disabledReason={limited && !disabled && !task.archived ? 'Available at '+new Date(floor).toLocaleTimeString() : undefined}
      onSelect={()=>void start()}>Run now</MenuItemButton>
    <MenuItemButton disabled={disabled} onSelect={()=>{trigger.current?.focus();setOpen(false);onDelete()}}>Delete task</MenuItemButton>
    </Menu>
    <button ref={closeRef} type="button" aria-label="Close details" onClick={onClose}><IconCloseOutlineRegular size={16}/></button></div>
    {error && <p role="alert">{error}</p>}
    <ReminderDetails row={task.row} exhausted={task.exhausted} now={now}/>
    {task.archived && <p role="status">The linked session is archived.</p>}
    <LinkedSession session={task.session} archived={task.archived}/>
  </aside>;
}

function DeleteTaskDialog({target,close,onDeleted}: {target:{session:string;id:string;prompt:string};close:()=>void;onDeleted:()=>void}) {
  const [busy,setBusy] = useState(false);
  const [error,setError] = useState('');
  async function remove() {
    setBusy(true);
    setError('');
    try {
      await input.command({action:'delete_scheduled_task',task:{session_id:target.session,id:target.id}});
      close();
      onDeleted();
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
      setBusy(false);
    }
  }
  return <Modal label="Delete this task?" className={dialogCss.dialog} initialFocus="[data-cancel]"
    fallbackFocus="[data-task-catalog-heading]" close={()=>{if(!busy)close()}} lightDismiss>
    <header><h2>Delete this task?</h2><button type="button" aria-label="Close deletion confirmation" disabled={busy} onClick={close}><IconCloseOutlineRegular/></button></header>
    <p>Future runs will stop. The linked session and messages already queued are kept.</p>
    <p className={css.confirmPrompt}>{target.prompt}</p>
    {error && <p role="alert">{error}</p>}
    <footer><Button data-cancel disabled={busy} onClick={close}>Cancel</Button>
      <Button disabled={busy} onClick={remove}>Confirm deletion</Button></footer>
  </Modal>;
}

function LinkedSession({session,archived}: {session:string;archived:boolean}) {
  const title = useView(view=>navigationTitle(view,session)) || session;
  function open() {
    const current = scheduleCatalog.getSnapshot()?.groups?.find(group=>group.position.session_id===session);
    if (current && !current.archived) void run(()=>input.open({action:'open',session}));
  }
  return <footer className={css.linkedSession}>
    <span>Linked session</span>
    <button type="button" disabled={archived} onClick={open}
      aria-label={title===session ? 'Linked session: open linked session' : 'Linked session '+title+': open linked session'}>
      <span>{title}</span><IconChevronRightOutlineRegular size={14}/>
    </button>
  </footer>;
}

export default function ScheduleCatalog({onClose,onDeleted}: {onClose:()=>void;onDeleted:()=>void}) {
  const catalog = useSyncExternalStore(scheduleCatalog.subscribe,scheduleCatalog.getSnapshot);
  const groups = catalog?.groups;
  const problem = catalog?.error;
  const [search,setSearch] = useState('');
  const [filter,setFilter] = useState<typeof filters[number]>('Active');
  const [selected,setSelected] = useState<{session:string;id:string}>();
  const [deleting,setDeleting] = useState<{session:string;id:string;prompt:string}>();
  const returnFocus = useRef<HTMLButtonElement|null>(null);
  const heading = useRef<HTMLHeadingElement>(null);
  const details = useRef<HTMLButtonElement>(null);
  const searchField = useRef<HTMLInputElement>(null);
  const detailId = useId();
  const all = useMemo(()=>groups?.flatMap(group=>group.reminders.map(({rule:row,executing})=>({
    session:group.position.session_id,row,title:row.prompt.trim().split('\n',1)[0],
    key:encodeURIComponent(group.position.session_id)+'/'+encodeURIComponent(row.id),
    archived:group.archived,exhausted:group.exhausted,executing,next:nextRunAt(row),status:reminderStatus(row,group.exhausted),
  }))).sort((a,b)=>a.next-b.next),[groups]);
  const task = selected && all?.find(item=>item.session===selected.session && item.row.id===selected.id);
  const showingDetails = !!task;
  const prefill = (text:string) => void run(()=>input.open({action:'new_conversation'},text));
  const query = search.trim();
  const searching = query.length > 0;
  const filtered = useMemo(()=>filter==='All' ? all : all?.filter(item=>filter===item.status),[all,filter]);
  const index = useMemo(()=>searching && filtered ? new TextSearchIndex(filtered,item=>({
    fuzzy:[item.title.slice(0,120)],substring:[item.session,item.row.prompt],
  })) : undefined,[filtered,searching]);
  const shown = useMemo(()=>index?.search(query) ?? filtered,[index,query,filtered]);
  const hasActiveRows = useMemo(()=>shown?.some(item=>item.status==='Active'),[shown]);
  const showCountdown = hasActiveRows || task?.status==='Active';
  const [now,setNow] = useState(Date.now);
  const floor = task?.row.last_delivery?.available_ms;
  useEffect(()=>{
    if (!floor) return;
    const delay = floor-Date.now();
    if (delay<=0) return;
    const timer = window.setTimeout(()=>setNow(Date.now()),delay);
    return ()=>window.clearTimeout(timer);
  },[floor]);
  useEffect(()=>{
    if (!showCountdown) return;
    setNow(Date.now());
    const timer = window.setInterval(()=>setNow(Date.now()),30_000);
    return ()=>window.clearInterval(timer);
  },[showCountdown]);
  useEffect(()=>{
    if (all && selected && !task) setSelected(undefined);
  },[all,selected,task]);
  useEffect(()=>{
    if (showingDetails) details.current?.focus();
    else if(returnFocus.current) (returnFocus.current.isConnected?returnFocus.current:heading.current)?.focus();
    else heading.current?.focus();
  },[showingDetails,selected]);
  return <section className={css.page} aria-label="Automation tasks" onKeyDown={event=>{
    if (!deleting && task && event.key==='Escape' && !event.defaultPrevented && !event.nativeEvent.isComposing) {
      event.preventDefault();
      event.stopPropagation();
      setSelected(undefined);
    }
  }}>
    <div className={css.catalog}><div className={css.content}>
      <header>
        <h1 ref={heading} data-task-catalog-heading tabIndex={-1}>Automation tasks</h1>
        <button className={css.create} type="button" onClick={()=>prefill(creationPrompt)}>Create in a session</button>
        <button type="button" onClick={onClose}>Close</button>
      </header>
      <div className={css.filters} role="group" aria-label="Task status">
        {filters.map(item=><button type="button" key={item} aria-pressed={filter===item} onClick={()=>setFilter(item)}>{item==='Active'?'Enabled':item}</button>)}
      </div>
      <div className={css.search}>
        <IconSearchOutlineRegular size={16}/>
        <input ref={searchField} type="search" aria-label="Search tasks" placeholder="Search automation tasks" value={search} onChange={e=>setSearch(e.target.value)}/>
        {search && <button type="button" aria-label="Clear search" onClick={()=>{setSearch('');searchField.current?.focus()}}><IconCloseOutlineRegular size={16}/></button>}
      </div>
      {problem && <p role="alert">{problem} <button type="button" onClick={()=>void loadScheduleCatalog()}>Retry</button></p>}
      {!all && !problem && <p role="status">Loading tasks…</p>}
      {shown && (shown.length ? <ul className={css.rows} aria-label="Task catalog">{shown.map(item=>{
        const metadataId = detailId+'-'+item.key;
        return <li key={item.key}>
          <button type="button" aria-label={item.title} aria-describedby={metadataId} aria-expanded={task===item}
            aria-controls={task===item ? detailId : undefined}
            onClick={event=>{returnFocus.current=event.currentTarget;setSelected({session:item.session,id:item.row.id})}}>
            <IconClockOutlineRegular size={16}/><span><strong>{item.title}</strong><small id={metadataId}>
              {item.status!=='Active' && item.status+' · '}{frequency(item.row)}
              {item.status==='Active' && <> · Next run <NextRun millis={item.next} now={now}/></>}
            </small></span>
          </button>
        </li>;
      })}</ul> : <div className={css.empty} role="status">{!query && <ScheduleEmptyIllustration/>}<p>{query?'No matching tasks':filter==='All'?'No tasks yet. Tasks created in your sessions appear here.':'No '+filter.toLowerCase()+' automation tasks'}</p></div>)}
      {shown?.length===0 && !query && (filter==='Active'||filter==='All') && <ScheduleRecommendations onUse={prefill}/>}
    </div></div>
    {task && <TaskDetail key={task.key} task={task} problem={!!problem} id={detailId} now={now} closeRef={details}
      onClose={()=>setSelected(undefined)} onDelete={()=>setDeleting({session:task.session,id:task.row.id,prompt:task.row.prompt})}/>}
    {deleting && <DeleteTaskDialog target={deleting} close={()=>setDeleting(undefined)} onDeleted={onDeleted}/>}
  </section>;
}
