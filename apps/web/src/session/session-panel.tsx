import {memo, useEffect, useId, useLayoutEffect, useRef, useState, type SyntheticEvent} from 'react';
import {createPortal} from 'react-dom';
import {writeClipboard} from '../../mounts.js';
import {Menu} from '../../vendor/dsh/dockkit-primitives/Menu.tsx';
import {Tooltip} from '../../vendor/dsh/dockkit-primitives/Tooltip.tsx';
import {IconCopyOutlineRegular, IconEllipsisOutlineRegular, IconFlatListOutlineRegular, IconFolderCloseRegular, IconFolderOpenOutlineRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {run, setNotice} from '../bridge.ts';
import {rowContextMenu} from '../row-context-menu.ts';
import {directory, hostDesktop} from '../directory-input.ts';
import type {BackgroundJob} from '../presentation-types.ts';
import {BackgroundJobs, liveJob, useJobFolds} from './background-jobs.tsx';
import css from './session-panel.module.css';

const hiddenDock = 'rsi.session-panel.hidden';
function dockVisible() {
  try { return localStorage.getItem(hiddenDock) !== '1'; } catch { return true; }
}

function WorkingFolder({path, local}: {path:string; local:boolean}) {
  const [menu, setMenu] = useState<{opener: HTMLElement; point: {x: number; y: number}}>();
  const [desktop, setDesktop] = useState<boolean>();
  const open = !!menu;
  useEffect(() => {
    if (!open || !local || desktop !== undefined) return;
    let current = true;
    void hostDesktop().then(value => { if (current) setDesktop(value); });
    return () => { current = false; };
  }, [open, local, desktop]);
  const label = useRef<HTMLSpanElement>(null);
  function showMenu(event: SyntheticEvent<HTMLElement>, point?: {x: number; y: number}) {
    if (event.defaultPrevented) return;
    event.preventDefault();
    event.stopPropagation();
    const opener = (event.target instanceof Element && event.target.closest('button')) || label.current;
    if (!opener) return;
    opener.focus({preventScroll: true});
    const rect = opener.getBoundingClientRect();
    setMenu({opener, point: point ?? {x: rect.left, y: rect.bottom}});
  }
  const trimmed = path.replace(/[/\\]+$/, '');
  const cut = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\')) + 1;
  const prefix = trimmed.slice(0, cut);
  const parent = /^(?:[/\\]|[A-Za-z]:[/\\])$/.test(prefix) ? prefix : prefix.slice(0, -1);
  const name = !trimmed || /^[A-Za-z]:$/.test(trimmed) ? path : trimmed.slice(cut);
  return <div className={css.row} {...rowContextMenu(showMenu)}>
    <IconFolderCloseRegular size={16} aria-hidden/>
    <Tooltip portal label={`Working folder\n${path}`} side="left" delayMs={500}>
      <span ref={label} className={css.folder} tabIndex={0} aria-haspopup="menu" aria-label={`Working folder: ${path}`}>
        <span>{name}</span>{parent && <span className={css.parent}>{parent}</span>}
      </span>
    </Tooltip>
    <button className={css.actions} type="button" aria-label="Working folder actions" aria-haspopup="menu"
      aria-expanded={!!menu} onClick={event => menu ? setMenu(undefined) : showMenu(event)}>
      <IconEllipsisOutlineRegular size={16} aria-hidden/>
    </button>
    {menu && <Menu open portal contextual autoFocus anchor={null} returnFocus={() => menu.opener}
      anchorPoint={menu.point} isExternalTrigger={node => node === menu.opener || menu.opener.contains(node)}
      items={[{id:'reveal', label:'Show in folder', icon:<IconFolderOpenOutlineRegular/>, disabled:!local || desktop !== true},
        {id:'copy', label:'Copy folder path', icon:<IconCopyOutlineRegular/>}]}
      onClose={() => setMenu(undefined)} onSelect={id => {
        menu.opener.focus({preventScroll: true});
        setMenu(undefined);
        if (id === 'reveal') void run(() => directory({action:'reveal', path}));
        else void run(async () => { await writeClipboard(path); setNotice('Copied'); });
      }}/>}
  </div>;
}

export const SessionPanel = memo(function SessionPanel({titleActions, path, local, chat, pane, generation, jobs, jobNotice}: {
  titleActions:HTMLElement | null; path:string; local:boolean; chat:boolean; pane:string; generation:string;
  jobs?:BackgroundJob[]|null; jobNotice?:string|null;
}) {
  const folds = useJobFolds();
  const running = jobs?.filter(liveJob).length ?? 0;
  const [fits, setFits] = useState(false), [pinned, setPinned] = useState(dockVisible);
  const [overlay, setOverlay] = useState(false);
  const probe = useRef<HTMLSpanElement>(null), trigger = useRef<HTMLButtonElement>(null), panel = useRef<HTMLElement>(null);
  const id = useId(), shown = chat && (fits ? pinned : overlay);
  useLayoutEffect(() => {
    const node = probe.current;
    if (!node) return;
    const update = () => setFits(node.getBoundingClientRect().width >= 1);
    update();
    const observer = new ResizeObserver(update);
    observer.observe(node);
    return () => observer.disconnect();
  }, [chat]);
  useEffect(() => {
    function changed(event: StorageEvent) {
      if (event.storageArea === localStorage && (event.key === hiddenDock || event.key === null)) setPinned(dockVisible());
    }
    window.addEventListener('storage', changed);
    return () => window.removeEventListener('storage', changed);
  }, []);
  useEffect(() => {
    if (!chat) setOverlay(false);
    if (!chat || !overlay || fits) return;
    function outside(event: PointerEvent) {
      const target = event.target;
      if (!(target instanceof Node) || panel.current?.contains(target) || trigger.current?.contains(target)) return;
      const menu = panel.current?.querySelector('[aria-controls]')?.getAttribute('aria-controls');
      if (!menu || !document.getElementById(menu)?.contains(target)) setOverlay(false);
    }
    function escape(event: KeyboardEvent) {
      if (event.key !== 'Escape' || event.defaultPrevented) return;
      event.preventDefault();
      setOverlay(false);
      if (panel.current?.contains(document.activeElement)) trigger.current?.focus();
    }
    document.addEventListener('pointerdown', outside);
    document.addEventListener('keydown', escape);
    return () => { document.removeEventListener('pointerdown', outside); document.removeEventListener('keydown', escape); };
  }, [chat, overlay, fits]);
  function toggle() {
    if (!fits) { setOverlay(!overlay); return; }
    setPinned(!pinned);
    try { if (pinned) localStorage.setItem(hiddenDock, '1'); else localStorage.removeItem(hiddenDock); } catch { /* The active pane retains the choice. */ }
  }
  return <div className={css.root}>
    {titleActions && createPortal(<Tooltip portal label={chat ? 'Session panel' : 'Session panel is available in Chat'} side="bottom" delayMs={500}>
      <button ref={trigger} type="button" className={css.trigger} disabled={!chat}
        aria-label={chat && !shown && running > 0 ? `Session panel Background jobs running: ${running}` : 'Session panel'}
        aria-expanded={shown} aria-controls={shown ? id : undefined} onClick={toggle}><IconFlatListOutlineRegular size={16} aria-hidden/>
        {chat && !shown && running > 0 && <span className={css.running} aria-hidden/>}</button>
    </Tooltip>, titleActions)}
    {chat && <span ref={probe} className={css.probe} aria-hidden/>}
    {shown && <aside ref={panel} id={id} className={css.panel} data-presentation={fits ? 'docked' : 'overlay'} aria-label="Session panel">
      <section><h2>Environment</h2><WorkingFolder path={path} local={local}/></section>
      {!!jobs?.length && <BackgroundJobs key={generation} pane={pane} generation={generation} rows={jobs} folds={folds}/>}
      {jobNotice && <p role="status">{jobNotice}</p>}
    </aside>}
  </div>;
});
