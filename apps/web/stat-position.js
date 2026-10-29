export function statPosition(anchor, panel, viewport) {
  const margin = 12, gap = 8;
  const left = Math.max(margin, Math.min(anchor.left, viewport.width - panel.width - margin));
  const above = anchor.top - panel.height - gap;
  const top = Math.max(margin, Math.min(above >= margin ? above : anchor.bottom + gap, viewport.height - panel.height - margin));
  return {left, top};
}
