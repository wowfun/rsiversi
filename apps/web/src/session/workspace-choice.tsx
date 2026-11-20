import {useView, run} from '../bridge.ts';
import {chooseWorkspace} from '../directory-picker.tsx';
import type {ComposerOwner} from './composer-state.ts';
import type {AttachedWorkspace} from '../presentation-types.ts';
import {ComposerMenu} from './composer-menu.tsx';
import {IconFolderOpenOutlineRegular, IconChevronDownOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {workspaceName} from '../../workspace-name.js';
export function WorkspaceChoice({owner, workspace, attached}: {owner:ComposerOwner; workspace?:string; attached?:AttachedWorkspace}) {
  const catalog = useView(view => view?.catalog);
  const registration=attached?.registration;
  const selected=attached
    ? registration?.status==='registered' ? {id:registration.id,name:registration.name,coordinates:attached.coordinates} : undefined
    : catalog?.workspaces.find(entry=>entry.id===workspace);
  const coordinates=attached?.coordinates??selected?.coordinates;
  const location=coordinates?.location.kind==='ssh'?`SSH ${coordinates.location.target}`:'Local';
  const label=coordinates?workspaceName(coordinates.path,selected?.name):'Choose workspace';
  const choices=selected&&!catalog?.workspaces.some(entry=>entry.id===selected.id)?[selected,...catalog?.workspaces??[]]:catalog?.workspaces??[];
  return <ComposerMenu name={`Conversation workspace, ${label}${coordinates?`, ${location}, ${coordinates.path}`:''}`} className="workspace-choice" focusSelection
    label={<><IconFolderOpenOutlineRegular/>{label}<IconChevronDownOutlineRegular/></>}>
    {close=><>
      {attached&&registration?.status!=='registered'&&<p>{registration?.status==='unregistered'?'Workspace is not registered.':'Workspace registration unavailable.'} {location}: {coordinates?.path}</p>}
      {choices.map(entry=><button key={entry.id} type="button" role="menuitemradio" aria-checked={entry.id===selected?.id} title={`${entry.coordinates.location.kind==='ssh'?`SSH ${entry.coordinates.location.target}`:'Local'}: ${entry.coordinates.path}`} disabled={owner.switching} onClick={()=>{close();if(entry.id!==selected?.id)void run(()=>owner.selectWorkspace(entry.id));}}><IconFolderOpenOutlineRegular/>{workspaceName(entry.coordinates.path, entry.name)}</button>)}
      <button type="button" role="menuitem" onClick={()=>{close();chooseWorkspace();}}>Open folder…</button>
    </>}
  </ComposerMenu>;
}
