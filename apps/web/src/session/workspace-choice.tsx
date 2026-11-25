import {useMemo, useRef, useState} from 'react';
import {run, useView} from '../bridge.ts';
import {chooseWorkspace} from '../directory-picker.tsx';
import {workspaceName} from '../../workspace-name.js';
import {Menu, type MenuEntry} from '../../vendor/dsh/dockkit-primitives/Menu.tsx';
import {IconChevronDownOutlineRegular, IconFolderCloseRegular, IconFolderOpenOutlineRegular, IconPlusOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {TextSearchIndex} from '../../vendor/dsh/text-search.ts';
import {distinguishingPathSuffixes} from '../../vendor/dsh/workspace/path-suffix.ts';
import type {ComposerOwner} from './composer-state.ts';
import type {AttachedWorkspace} from '../presentation-types.ts';
import css from './composer-menu.module.css';

export function WorkspaceChoice({owner, workspace, attached}: {owner:ComposerOwner; workspace?:string; attached?:AttachedWorkspace}) {
  const workspaces = useView(view => view?.catalog?.workspaces);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const trigger = useRef<HTMLButtonElement>(null);
  const catalogSelection = attached ? undefined : workspaces?.find(row => row.id === workspace);
  const selected = attached
    ? attached.registration?.status === 'registered' ? attached.registration : undefined
    : catalogSelection;
  const coordinates = attached?.coordinates ?? catalogSelection?.coordinates;
  const location = coordinates?.location.kind === 'ssh' ? `SSH ${coordinates.location.target}` : 'Local';
  const label = coordinates ? workspaceName(coordinates.path, selected?.name) : 'Choose workspace';
  const rows = useMemo(() => {
    if (!open) return [];
    let choices = workspaces ?? [];
    const registration = attached?.registration;
    if (attached && registration?.status === 'registered' && !choices.some(row => row.id === registration.id)) {
      choices = [{id:registration.id, name:registration.name, coordinates:attached.coordinates}, ...choices];
    }
    return choices.map(({id, name, coordinates}) => {
      const {path, location} = coordinates;
      return {id, title:workspaceName(path, name), path:location.kind === 'ssh' ? `${location.target}:${path}` : path};
    });
  }, [open, workspaces, attached]);
  const search = useMemo(() => rows.length > 1
    ? new TextSearchIndex(rows, row => ({fuzzy:[row.title], substring:[row.path]}))
    : undefined, [rows]);
  const suffixes = useMemo(() => distinguishingPathSuffixes(rows), [rows]);
  const items: MenuEntry[] = (search?.search(query) ?? rows).map(row => ({
    id:row.id, current:row.id === selected?.id, disabled:owner.switching,
    icon:<IconFolderCloseRegular size={16}/>,
    label:suffixes.has(row.id)
      ? <span className={css.workspaceLabel} title={row.path}><span>{row.title}</span>{' '}<span>{suffixes.get(row.id)}</span></span>
      : <span title={row.path}>{row.title}</span>,
  }));
  if (attached && attached.registration?.status !== 'registered' && !query.trim()) {
    const status = attached.registration?.status === 'unregistered' ? 'Workspace is not registered.' : 'Workspace registration unavailable.';
    items.unshift({type:'label', id:'registration', text:`${status} ${location}: ${coordinates?.path}`});
  }
  const close = () => {setOpen(false); setQuery('');};
  return <Menu open={open} side="top" portal anchorRef={trigger}
    anchor={<button ref={trigger} type="button" className={`workspace-choice ${css.trigger}`}
      aria-label={`Conversation workspace, ${label}${coordinates ? `, ${location}, ${coordinates.path}` : ''}`}
      aria-haspopup="menu" aria-expanded={open}
      onClick={() => {if (open) close(); else if (!attached && workspaces?.length === 0) chooseWorkspace(); else setOpen(true);}}>
      <IconFolderOpenOutlineRegular/>{label}<IconChevronDownOutlineRegular/>
    </button>}
    items={items} selectedId={selected?.id} maxVisibleRows={6}
    search={search ? {label:'Search workspaces', value:query, onChange:setQuery, emptyMessage:'No matching workspaces', skipAutoFocusOnTouch:true} : undefined}
    footer={[{id:'', label:'Add workspace…', icon:<IconPlusOutlineRegular size={16}/>}]} onClose={close}
    onSelect={id => {
      close();
      if (id === '') chooseWorkspace();
      else if (id !== selected?.id) void run(() => owner.selectWorkspace(id));
    }}/>;
}
