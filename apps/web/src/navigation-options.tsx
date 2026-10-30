import {useRef, type ComponentType, type KeyboardEvent} from 'react'
import {IconArchiveCheckOutlineRegular, IconArchiveOffOutlineRegular, IconCheckOutlineRegular, IconChevronsUpDownOutlineRegular, IconClockOutlineRegular, IconFlatListOutlineRegular, IconFolderCloseRegular, IconQueueOutlineRegular, IconSlidersTwoOutlineRegular, IconWorkspaceTreeOutlineRegular} from '../vendor/dsh/dockkit-primitives/icons/index.tsx'
import {closeDisclosure} from '../disclosure.js'
import {run} from './bridge.ts'
import type {useDeviceNavigation} from './navigation-device.ts'
import css from './navigation.module.css'

export type ConversationFilter = 'active'|'all'|'archived'|'attention'
export function NavigationOptions({device,filter,onFilter,refresh}:{
  device:ReturnType<typeof useDeviceNavigation>;filter:ConversationFilter;
  onFilter(value:ConversationFilter):Promise<unknown>;refresh():Promise<unknown>
}) {
  const disclosure=useRef<HTMLDetailsElement>(null)
  const choices=()=>Array.from(disclosure.current?.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]:not(:disabled)')??[])
  const keyboard=(event:KeyboardEvent)=>{
    if(!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return
    event.preventDefault()
    const rows=choices(),index=rows.indexOf(document.activeElement as HTMLButtonElement)
    rows[event.key==='Home'?0:event.key==='End'?rows.length-1:(index+(event.key==='ArrowDown'?1:-1)+rows.length)%rows.length]?.focus()
  }
  const choice=(label:string,selected:boolean,Icon:ComponentType<{size?:number}>,action:()=>Promise<unknown>,disabled=false)=><button key={label} type="button" role="menuitemradio" aria-checked={selected} disabled={disabled} onClick={event=>{closeDisclosure(event.currentTarget,true);void run(action)}}><Icon size={16}/><span>{label}</span>{selected&&<IconCheckOutlineRegular size={16}/>}</button>
  return <details ref={disclosure} className={`navigation-options ${css.options}`}>
    <summary aria-label="Navigation options" aria-haspopup="menu" title="View options" onKeyDown={event=>{if(event.key==='ArrowDown'){event.preventDefault();disclosure.current!.open=true;choices()[0]?.focus()}}}><IconSlidersTwoOutlineRegular size={16}/></summary>
    <div className={css.menu} role="menu" aria-label="Navigation options" onKeyDown={keyboard}>
      <div role="group" aria-label="Group by">
      <div className={css.menuLabel}>Group by</div>
      {choice('By workspace',device.preferences.view==='workspace',IconFolderCloseRegular,()=>device.view('workspace'),!device.ready)}
      {choice('By workspace tree',device.preferences.view==='workspace_tree',IconWorkspaceTreeOutlineRegular,()=>device.view('workspace_tree'),!device.ready)}
      {choice('Single list',device.preferences.view==='flat',IconFlatListOutlineRegular,()=>device.view('flat'),!device.ready)}
      </div><div role="separator"/>
      <div role="group" aria-label="Order by">
      <div className={css.menuLabel}>Order by</div>
      {choice('Manual',device.preferences.sessionOrder==='manual',IconChevronsUpDownOutlineRegular,()=>device.mode('manual'),!device.ready)}
      {choice('Recently updated',device.preferences.sessionOrder==='updated',IconClockOutlineRegular,()=>device.mode('updated'),!device.ready)}
      </div><div role="separator"/>
      <div role="group" aria-label="Filter conversations">
      <div className={css.menuLabel}>Filter conversations</div>
      {choice('Hide archived',filter==='active',IconArchiveOffOutlineRegular,()=>onFilter('active'))}
      {choice('All conversations',filter==='all',IconQueueOutlineRegular,()=>onFilter('all'))}
      {choice('Archived only',filter==='archived',IconArchiveCheckOutlineRegular,()=>onFilter('archived'))}
      {choice('Needs attention',filter==='attention',IconClockOutlineRegular,()=>onFilter('attention'))}
      </div><div role="separator"/>
      <button id="refresh" type="button" role="menuitem" onClick={event=>{closeDisclosure(event.currentTarget,true);void run(refresh)}}>Refresh</button>
      {device.preferences.workspaceOrder==='manual'&&<button type="button" role="menuitem" onClick={event=>{closeDisclosure(event.currentTarget,true);void run(device.workspaceUpdated)}}>Reset workspace order</button>}
    </div>
  </details>
}
