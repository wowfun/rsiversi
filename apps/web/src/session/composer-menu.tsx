import {useId, useLayoutEffect, useRef, useState, type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {watchPanelPosition} from '../../stat-position.js';
import anchor from './anchored-panel.module.css';
import css from './composer-menu.module.css';

export function ComposerMenu({name, label, children, onOpen, className = '', disabled = false, focusSelection = false}: {
  focusSelection?: boolean; name: string; label: ReactNode; className?: string; disabled?: boolean;
  onOpen?: () => void;
  children: (close: () => void) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null), panel = useRef<HTMLDivElement>(null);
  const id = useId();
  const anchorName = `--rsi-menu-${id.replaceAll(':', '')}`;
  const close = () => { setOpen(false); trigger.current?.focus({preventScroll:true}); };
  useLayoutEffect(() => {
    if (!open) return;
    const outside = (event: Event) => {
      if (event.target instanceof Node && !panel.current?.contains(event.target) && !trigger.current?.contains(event.target)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if(event.key==='Escape'&&!event.isComposing){event.preventDefault();event.stopPropagation();close();}
    };
    const stopPositioning = watchPanelPosition(trigger.current!, panel.current!);
    const selected=focusSelection ? panel.current?.querySelector<HTMLElement>('button[aria-checked="true"]:not(:disabled)') ?? panel.current?.querySelector<HTMLElement>('button:not(:disabled)') : null;
    (selected??panel.current)?.focus({preventScroll:true});
    document.addEventListener('pointerdown',outside); document.addEventListener('focusin',outside);
    document.addEventListener('keydown',escape);
    return () => {
      stopPositioning?.();
      document.removeEventListener('pointerdown',outside); document.removeEventListener('focusin',outside);
      document.removeEventListener('keydown',escape);
    };
  },[open]);
  return <>
    <button type="button" ref={trigger} style={{anchorName}} className={`${css.trigger} ${className}`} aria-label={name} aria-haspopup="menu" aria-expanded={open} aria-controls={open ? id : undefined} disabled={disabled}
      onClick={() => { if(open) close(); else { setOpen(true); void onOpen?.(); } }}>
      {label}
    </button>
    {open && createPortal(<div ref={panel} id={id} role="menu" aria-label={name} tabIndex={-1} style={{positionAnchor:anchorName}} className={`${anchor.panel} ${css.panel}`}
      onKeyDown={event => {
        if(event.defaultPrevented||event.nativeEvent.isComposing)return;
        if(event.key==='Escape') {event.preventDefault();event.stopPropagation();close();return;}
        if(!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return;
        if(event.target instanceof HTMLInputElement && ['Home','End'].includes(event.key))return;
        const items=[...panel.current!.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')].filter(item=>item.getClientRects().length);
        if(!items.length)return;
        event.preventDefault();
        const index=items.indexOf(document.activeElement as HTMLButtonElement);
        const next=event.key==='Home'?0:event.key==='End'?items.length-1:event.key==='ArrowDown'?(index+1)%items.length:index<0?items.length-1:(index+items.length-1)%items.length;
        items[next].focus();
      }}>{children(close)}</div>,document.body)}
  </>;
}
