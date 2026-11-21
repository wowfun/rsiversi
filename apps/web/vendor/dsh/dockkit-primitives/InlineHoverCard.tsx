import {useCallback, useLayoutEffect, useRef, useState, type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {usePointerGrace} from './pointer-grace.ts';
import {overlayTopMargin} from './overlay-top-margin.ts';
import css from './InlineHoverCard.module.css';

/** The inline image-preview variant of HoverCard. Only an open card tracks its anchor. */
export function InlineHoverCard({anchor, children}: {anchor: ReactNode; children: ReactNode}) {
  const root = useRef<HTMLSpanElement>(null);
  const card = useRef<HTMLSpanElement>(null);
  const dwell = useRef<ReturnType<typeof setTimeout>>();
  const [open, setOpen] = useState(false);
  const close = useCallback(() => { clearTimeout(dwell.current); setOpen(false); }, []);
  const grace = usePointerGrace(close);
  useLayoutEffect(() => () => clearTimeout(dwell.current), []);
  useLayoutEffect(() => {
    if (!open) return;
    const trigger = root.current!, content = card.current!;
    function place() {
      if (trigger.closest('[inert],[hidden]')) { close(); return; }
      const r = trigger.getBoundingClientRect();
      const margin = overlayTopMargin(8, r, 8);
      const belowTop = Math.max(margin, r.bottom + 8);
      const below = Math.max(0, innerHeight - belowTop - 8);
      const above = Math.max(0, r.top - 8 - margin);
      const onTop = content.scrollHeight > below && above > below;
      const top = onTop ? r.top - 8 - Math.min(content.scrollHeight, above) : belowTop;
      content.style.left = Math.max(8, Math.min(r.left, innerWidth - content.offsetWidth - 8)) + 'px';
      content.style.top = top + 'px';
      content.style.maxHeight = (onTop ? above : below) + 'px';
    }
    function dismiss(event: KeyboardEvent) {
      if (event.key === 'Escape') { event.stopPropagation(); close(); }
    }
    place();
    const size = new ResizeObserver(place);
    size.observe(trigger);
    size.observe(content);
    const visibility = new MutationObserver(place);
    for (let parent = trigger.parentElement; parent; parent = parent.parentElement)
      visibility.observe(parent, {attributes: true, attributeFilter: ['inert','hidden']});
    window.addEventListener('scroll', place, true);
    window.addEventListener('resize', place);
    window.addEventListener('keydown', dismiss, true);
    return () => {
      size.disconnect();
      visibility.disconnect();
      window.removeEventListener('scroll', place, true);
      window.removeEventListener('resize', place);
      window.removeEventListener('keydown', dismiss, true);
    };
  }, [open, close]);
  return <span ref={root} onFocus={event => {
    if (event.target.matches(':focus-visible')) { grace.cancel(); setOpen(true); }
  }} onBlur={close} onPointerEnter={event => {
    if (event.pointerType === 'touch' || (event.pointerType === 'mouse' && !matchMedia('(any-hover: hover)').matches)) return;
    grace.cancel();
    if (!open) { clearTimeout(dwell.current); dwell.current = setTimeout(() => setOpen(true), 500); }
  }} onPointerLeave={() => {
    clearTimeout(dwell.current);
    if (open) grace.arm();
  }} onClickCapture={event => { if (!card.current?.contains(event.target as Node)) close(); }}>
    {anchor}
    {open && createPortal(<span className={css.media} ref={card} onPointerEnter={grace.cancel}
      onPointerLeave={grace.arm}>{children}</span>, document.body)}
  </span>;
}
