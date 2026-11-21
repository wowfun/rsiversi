export function statPosition(anchor, panel, viewport) {
  const margin = 12, gap = 8;
  const left = Math.max(margin, Math.min(anchor.left, viewport.width - panel.width - margin));
  const above = anchor.top - panel.height - gap;
  const top = Math.max(margin, Math.min(above >= margin ? above : anchor.bottom + gap, viewport.height - panel.height - margin));
  return {left, top};
}

// Older WebViews use the same anchor and viewport insets as CSS positioning.
export function watchPanelPosition(trigger, panel) {
  if (CSS.supports('position-try-fallbacks: --rsi-panel-window') && CSS.supports('top: anchor(bottom)')) return;
  const place = () => {
    const {left, top} = statPosition(trigger.getBoundingClientRect(), panel.getBoundingClientRect(),
      {width: innerWidth, height: innerHeight});
    Object.assign(panel.style, {left: `${left}px`, top: `${top}px`});
  };
  place();
  const observer = new ResizeObserver(place);
  observer.observe(panel);
  window.addEventListener('resize', place);
  window.addEventListener('scroll', place, true);
  return () => {
    observer.disconnect();
    window.removeEventListener('resize', place);
    window.removeEventListener('scroll', place, true);
  };
}
