import {useRef} from 'react';
import {useView, run} from '../bridge.ts';
import {chooseWorkspace} from '../directory-picker.tsx';
import {closeDisclosure} from '../../disclosure.js';
import type {ComposerOwner} from './composer-state.ts';
import {IconFolderOpenOutlineRegular, IconChevronDownOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {workspaceName} from '../../workspace-name.js';
export function WorkspaceChoice({owner, workspace}: {owner:ComposerOwner; workspace?:string}) {
  const catalog = useView(view => view?.catalog), menu = useRef<HTMLDetailsElement>(null);
  const selected = catalog?.workspaces.find(entry=>entry.id===workspace) ?? catalog?.workspaces[0];
  return <details ref={menu} className="workspace-choice">
    <summary aria-label="Conversation workspace"><IconFolderOpenOutlineRegular/>{selected ? workspaceName(selected.coordinates.path, selected.name) : 'Choose workspace'}<IconChevronDownOutlineRegular/></summary>
    <div className="workspace-choice-menu" role="menu" aria-label="Conversation workspace">
      {catalog?.workspaces.map(entry=><button key={entry.id} type="button" role="menuitemradio" aria-checked={entry.id===selected?.id} title={entry.coordinates.path} disabled={owner.switching} onClick={event=>{closeDisclosure(event.currentTarget);void run(()=>owner.selectWorkspace(entry.id));}}><IconFolderOpenOutlineRegular/>{workspaceName(entry.coordinates.path, entry.name)}</button>)}
      <button type="button" role="menuitem" onClick={event=>{closeDisclosure(event.currentTarget);chooseWorkspace();}}>Open folder…</button>
    </div>
  </details>;
}
