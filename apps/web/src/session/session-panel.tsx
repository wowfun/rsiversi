import {memo, useEffect, useId, useLayoutEffect, useRef, useState} from 'react';
import {Tooltip} from '../../vendor/dsh/dockkit-primitives/Tooltip.tsx';
import {IconFlatListOutlineRegular, IconFolderCloseRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import css from './session-panel.module.css';

const hiddenDock = 'rsi.session-panel.hidden';
function dockVisible() {
  try { return localStorage.getItem(hiddenDock) !== '1'; } catch { return true; }
}

function WorkingFolder({path}: {path:string}) {
  const trimmed = path.replace(/[/\\]+$/, '');
  const cut = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\')) + 1;
  const directory = trimmed.slice(0, cut);
  const parent = /^(?:[/\\]|[A-Za-z]:[/\\])$/.test(directory) ? directory : directory.slice(0, -1);
  const name = !trimmed || /^[A-Za-z]:$/.test(trimmed) ? path : trimmed.slice(cut);
  return <div className={css.row}>
    <IconFolderCloseRegular size={16} aria-hidden/>
    <Tooltip portal label={`Working folder\n${path}`} side="left" delayMs={500}>
      <span className={css.folder} tabIndex={0} aria-label={`Working folder: ${path}`}>
        <span>{name}</span>{parent && <span className={css.parent}>{parent}</span>}
      </span>
    </Tooltip>
  </div>;
}

export const SessionPanel = memo(function SessionPanel({path, chat}: {path:string; chat:boolean}) {
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
      if (event.target instanceof Node && !panel.current?.contains(event.target) && !trigger.current?.contains(event.target)) setOverlay(false);
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
    <Tooltip portal label={chat ? 'Session panel' : 'Session panel is available in Chat'} side="bottom" delayMs={500}>
      <button ref={trigger} type="button" className={css.trigger} disabled={!chat} aria-label="Session panel"
        aria-expanded={shown} aria-controls={shown ? id : undefined} onClick={toggle}><IconFlatListOutlineRegular size={16} aria-hidden/></button>
    </Tooltip>
    {chat && <span ref={probe} className={css.probe} aria-hidden/>}
    {shown && <aside ref={panel} id={id} className={css.panel} data-presentation={fits ? 'docked' : 'overlay'} aria-label="Session panel">
      <h2>Environment</h2><WorkingFolder path={path}/>
    </aside>}
  </div>;
});
