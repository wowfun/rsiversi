import {useEffect, useLayoutEffect, useRef, useState} from 'react';
import {Button} from './button.tsx';
import {input, run, source, useView} from './bridge.ts';
import {PluginDetails, PluginPagination, usePluginCommand} from './plugins.tsx';
import {IconChevronLeftOutlineMedium, IconCloseOutlineRegular, IconPluginPinwheelOutlineMedium, IconRefreshOutlineRegular, IconSearchOutlineRegular, IconSettingsOutlineRegular} from '../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './plugin-manager.module.css';

const hostInventory={kind:'select',target:{kind:'host'}};
export default function PluginManager({onClose,onSettings}:{onClose:()=>void;onSettings:()=>void}) {
  const view=useView(value=>value?.plugins);
  const {busy,problem,command}=usePluginCommand();
  const [selected,setSelected]=useState<string>();
  const heading=useRef<HTMLHeadingElement>(null);
  const root=useRef<HTMLElement>(null);
  const page=view?.target.kind==='host'?view.page:null;
  const row=selected && page?.plugins.find(entry=>entry.instance===selected);
  useEffect(()=>{
    const loaded=source.getSnapshot()?.plugins;
    if (loaded?.target.kind!=='host' || loaded.page?.offset!==0 || loaded.diagnostic) void command(hostInventory);
  },[command]);
  useLayoutEffect(()=>{heading.current?.focus({preventScroll:true})},[selected]);
  function back(instance:string) {
    setSelected(undefined);
    requestAnimationFrame(()=>root.current?.querySelector<HTMLButtonElement>(`[data-instance="${CSS.escape(instance)}"]`)?.focus());
  }

  return <section ref={root} className={css.page} aria-label="Plugins" aria-busy={busy} data-plugin-panel>
    <div className={css.content}>
      <header>
        {row && <Button aria-label="Back to plugins" onClick={()=>back(row.instance)}><IconChevronLeftOutlineMedium size={20}/></Button>}
        <h1 ref={heading} tabIndex={-1}>{row?row.instance:'Plugins'}</h1>
        <Button variant="outline" size="sm" icon={<IconRefreshOutlineRegular size={13}/>} disabled={busy} onClick={()=>void command(hostInventory)}>Refresh</Button>
        <Button aria-label="Close plugins" onClick={onClose}><IconCloseOutlineRegular/></Button>
      </header>
      {(problem || view?.diagnostic) && <p role="alert">{problem || view?.diagnostic}</p>}
      {row ? <div className={css.detail}><PluginDetails row={row}/></div> : <>
        <section aria-label="Built-in plugin settings">
          <h2>Built-in plugin settings</h2>
          {view?.exa_available && <button className={css.card} onClick={()=>void run(()=>input.command({action:'settings_read',namespace:'rsi.retrieval'}))}>
            <span className={css.icon}><IconSearchOutlineRegular size={22}/></span>
            <span><strong>Web search and fetch</strong><small>Configure web retrieval tools</small></span>
          </button>}
          <button className={css.card} onClick={onSettings}>
            <span className={css.icon}><IconSettingsOutlineRegular size={22}/></span>
            <span><strong>Host configuration and diagnostics</strong><small>Inspect profiles, connections and conversation plugins</small></span>
          </button>
        </section>
        <section aria-label="Host plugins"><h2>Host plugins</h2>
          {!page && !problem && !view?.diagnostic && <p role="status">Loading plugin inventory…</p>}
          {page && <>
            {page.total===0 && <p>No Host plugins configured.</p>}
            <ul className={css.rows}>{page.plugins.map(entry=><li key={entry.instance}>
              <button className={css.card} data-instance={entry.instance} aria-label={`View ${entry.instance}`} onClick={()=>setSelected(entry.instance)}>
                <span className={css.icon}><IconPluginPinwheelOutlineMedium size={22}/></span>
                <span><strong>{entry.instance}</strong><small>{entry.desired_plugin ?? entry.observed?.plugin}</small></span>
                <span className={css.status}>{entry.observed?.state.replaceAll('_',' ') ?? (entry.enabled?'Not observed':'Disabled')}</span>
              </button>
            </li>)}</ul>
            {view && (page.offset>0 || page.next_offset!==null) && <PluginPagination page={page} busy={busy} go={offset=>void command({kind:'page',ticket:view.ticket,offset})}/>}
          </>}
        </section>
      </>}
    </div>
  </section>;
}
