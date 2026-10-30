import {useId, useLayoutEffect, useRef, useState, type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {statPosition} from '../../stat-position.js';
import css from './metrics.module.css';

export function StatPopover({label, name, icon, headline, children, compact = false}: {
  label: ReactNode;
  name: string;
  icon?: ReactNode;
  headline?: ReactNode;
  children: ReactNode;
  compact?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const id = useId();
  useLayoutEffect(() => {
    if (!open) return;
    const place = () => {
      if (!trigger.current || !panel.current) return;
      const position = statPosition(trigger.current.getBoundingClientRect(), panel.current.getBoundingClientRect(), {width: innerWidth, height: innerHeight});
      panel.current.style.left = `${position.left}px`;
      panel.current.style.top = `${position.top}px`;
      panel.current.style.visibility = 'visible';
    };
    const outside = (event: Event) => {
      const target = event.target;
      if (target instanceof Node && !panel.current?.contains(target) && !trigger.current?.contains(target)) setOpen(false);
    };
    const key = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      event.preventDefault();
      setOpen(false);
      trigger.current?.focus({preventScroll: true});
    };
    place();
    panel.current?.focus({preventScroll: true});
    const observer = new ResizeObserver(place);
    observer.observe(panel.current!);
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    document.addEventListener('pointerdown', outside);
    document.addEventListener('focusin', outside);
    document.addEventListener('keydown', key);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
      document.removeEventListener('pointerdown', outside);
      document.removeEventListener('focusin', outside);
      document.removeEventListener('keydown', key);
    };
  }, [open]);
  return <>
    <button ref={trigger} type="button" className={`${css.pill}${compact ? ` ${css.contextPill}` : ''}`} aria-haspopup="dialog" aria-expanded={open} aria-controls={open ? id : undefined} onClick={() => setOpen(value => !value)}>
      {icon}<span className={css.label}>{label}</span>
    </button>
    {open && createPortal(<div ref={panel} id={id} role="dialog" aria-label={name} tabIndex={-1} className={`${css.panel}${compact ? ` ${css.context}` : ''}`}>
      <div className={css.title}><span>{icon}{name}</span>{headline != null && <strong>{headline}</strong>}</div>
      <div className={css.rule}/>
      {children}
    </div>, document.body)}
  </>;
}
