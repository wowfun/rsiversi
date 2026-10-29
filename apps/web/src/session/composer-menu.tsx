import {useId, useLayoutEffect, useRef, useState, type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {statPosition} from '../../stat-position.js';
import css from './composer-menu.module.css';

// Children stay mounted so runtime-owned attachment controls retain their listeners.
export function ComposerMenu({name, label, children, onOpen, className = '', disabled = false}: {
  name: string; label: ReactNode; className?: string; disabled?: boolean;
  onOpen?: () => Promise<unknown>;
  children: (close: () => void, open: boolean) => ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null), panel = useRef<HTMLDivElement>(null);
  const id = useId();
  const close = () => { setOpen(false); trigger.current?.focus({preventScroll:true}); };
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      if (!trigger.current || !panel.current) return;
      const position = statPosition(trigger.current.getBoundingClientRect(), panel.current.getBoundingClientRect(), {width:innerWidth,height:innerHeight});
      Object.assign(panel.current.style, {left:`${position.left}px`,top:`${position.top}px`});
    };
    const outside = (event: Event) => {
      if (event.target instanceof Node && !panel.current?.contains(event.target) && !trigger.current?.contains(event.target)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if(event.key==='Escape'&&!event.isComposing){event.preventDefault();event.stopPropagation();close();}
    };
    place();
    panel.current?.focus({preventScroll:true});
    const observer = new ResizeObserver(place); observer.observe(panel.current!);
    window.addEventListener('resize',place); window.addEventListener('scroll',place,true);
    document.addEventListener('pointerdown',outside); document.addEventListener('focusin',outside);
    document.addEventListener('keydown',escape);
    return () => {
      observer.disconnect(); window.removeEventListener('resize',place); window.removeEventListener('scroll',place,true);
      document.removeEventListener('pointerdown',outside); document.removeEventListener('focusin',outside);
      document.removeEventListener('keydown',escape);
    };
  },[open]);
  return <>
    <button type="button" ref={trigger} className={`${css.trigger} ${className}`} aria-label={name} aria-haspopup="menu" aria-expanded={open} aria-controls={id} disabled={disabled}
      onClick={() => { if(open) close(); else { setOpen(true); void onOpen?.(); } }}>
      {label}
    </button>
    {createPortal(<div ref={panel} id={id} role="menu" aria-label={name} tabIndex={-1} hidden={!open} className={css.panel}
      onKeyDown={event => {
        if(event.key==='Escape') {event.preventDefault();event.stopPropagation();close();return;}
        if(!['ArrowDown','ArrowUp','Home','End'].includes(event.key))return;
        if(event.target instanceof HTMLInputElement && ['Home','End'].includes(event.key))return;
        const items=[...panel.current!.querySelectorAll<HTMLButtonElement>('button:not(:disabled)')].filter(item=>item.getClientRects().length);
        if(!items.length)return;
        event.preventDefault();
        const index=items.indexOf(document.activeElement as HTMLButtonElement);
        const next=event.key==='Home'?0:event.key==='End'?items.length-1:event.key==='ArrowDown'?(index+1)%items.length:(index+items.length-1)%items.length;
        items[next].focus();
      }}>{children(close,open)}</div>,document.body)}
  </>;
}
