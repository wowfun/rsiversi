// Display-only visibility and scroll calculations shared by React presentation and tests.
export function turnVisibility(turn, mode, expanded) {
  const protectedHistory = !turn.foldable;
  const collapse = !protectedHistory && mode !== 'verbose' && !expanded && (!turn.running || mode === 'compact');
  const candidate = new Set(turn.candidate);
  const hidden = new Set(collapse ? turn.process.filter(key=>!candidate.has(key)) : []);
  if (turn.terminal && turn.foldable && !turn.running && turn.answer.length && mode !== 'verbose') hidden.add(turn.terminal);
  return {hidden,
    expanded:protectedHistory || mode==='verbose' || mode==='detailed' || expanded};
}
export function awayFromBottom(container) {
  return container.scrollHeight - container.clientHeight - Math.max(0, container.scrollTop) > 70;
}
export function readingPosition(container, readingSummary) {
  const previousHeight=container.scrollHeight;
  const atEnd=!readingSummary && previousHeight-container.clientHeight-container.scrollTop<70;
  const anchors=atEnd ? [] : readingAnchor(container);
  return {previousHeight,atEnd,anchors};
}
function readingAnchor(container) {
  const top=container.getBoundingClientRect().top;
  const anchors=[];
  for(const node of container.children) {
    if(node.hidden)continue;
    const rect=node.getBoundingClientRect();
    if(rect.bottom>top)anchors.push({node,offset:rect.top-top});
  }
  return anchors;
}
export function restoreAnchor(container, anchors) {
  const anchor=anchors.find(item=>item.node.isConnected && !item.node.hidden && item.node.getClientRects().length);
  if(anchor)container.scrollTop += anchor.node.getBoundingClientRect().top-container.getBoundingClientRect().top-anchor.offset;
}
