import {useId, useLayoutEffect, useRef, useState, type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {watchPanelPosition} from '../../stat-position.js';
import anchor from './anchored-panel.module.css';
import css from './metrics.module.css';

export function StatPopover({label, accessibleLabel, name, icon, headline, children, compact = false}: {
  label: ReactNode;
  accessibleLabel?: string;
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
  const anchorName = `--rsi-stat-${id.replaceAll(':', '')}`;
  useLayoutEffect(() => {
    if (!open) return;
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
    const stopPositioning = watchPanelPosition(trigger.current!, panel.current!);
    panel.current?.focus({preventScroll: true});
    document.addEventListener('pointerdown', outside);
    document.addEventListener('focusin', outside);
    document.addEventListener('keydown', key);
    return () => {
      stopPositioning?.();
      document.removeEventListener('pointerdown', outside);
      document.removeEventListener('focusin', outside);
      document.removeEventListener('keydown', key);
    };
  }, [open]);
  return <>
    <button ref={trigger} type="button" style={{anchorName}} className={`${css.pill}${compact ? ` ${css.contextPill}` : ''}`} aria-label={accessibleLabel} title={accessibleLabel} aria-haspopup="dialog" aria-expanded={open} aria-controls={open ? id : undefined} onClick={() => setOpen(value => !value)}>
      {icon}<span className={css.label}>{label}</span>
    </button>
    {open && createPortal(<div ref={panel} id={id} role="dialog" aria-label={name} tabIndex={-1} style={{positionAnchor:anchorName}} className={`${anchor.panel} ${css.panel}${compact ? ` ${css.context}` : ''}`}>
      {!compact && <><div className={css.title}><span>{icon}{name}</span>{headline != null && <strong>{headline}</strong>}</div>
      <div className={css.rule}/></>}
      {children}
    </div>, document.body)}
  </>;
}
