// Display-only visibility and scroll calculations shared by React presentation and tests.
const policies = {
  compact: {foldCompleted:true, expandRunning:false, expandCompleted:false, liveDetail:false, reasoningPreview:false, foldRunning:true},
  standard: {foldCompleted:true, expandRunning:false, expandCompleted:false, liveDetail:true, reasoningPreview:true, foldRunning:false},
  detailed: {foldCompleted:true, expandRunning:true, expandCompleted:false, liveDetail:true, reasoningPreview:true, foldRunning:false},
  verbose: {foldCompleted:false, expandRunning:true, expandCompleted:true, liveDetail:false, reasoningPreview:true, foldRunning:false},
};
export const presentationPolicy = mode => policies[mode] ?? policies.standard;
export function reasoningPreview(text, running) {
  if (!running) {
    const newline = text.indexOf('\n');
    return (newline < 0 ? text : text.slice(0, newline)).trimEnd().replaceAll('**', '');
  }
  const separators = /\r?\n(?:[\t ]*\r?\n)+/g;
  let start = 0, summary = '';
  for (;;) {
    const separator = separators.exec(text);
    const newline = text.indexOf('\n', start);
    if (newline >= 0 && (!separator || newline <= separator.index + separator[0].indexOf('\n'))) {
      const first = text.slice(start, newline).trim();
      if (first) summary = first;
    }
    if (!separator) return summary.replaceAll('**', '');
    start = separators.lastIndex;
  }
}
const noReplies = new Set();
export function turnVisibility(turn, mode, expanded, replies = noReplies) {
  const policy = presentationPolicy(mode);
  const protectedHistory = !turn.foldable;
  const collapse = !protectedHistory && policy.foldCompleted && !expanded && (!turn.running || policy.foldRunning);
  const candidate = new Set(turn.candidate);
  const hidden = new Set(collapse ? turn.process.filter(key=>!candidate.has(key) && !replies.has(key)) : []);
  if (turn.terminal && turn.foldable && !turn.running && turn.answer.length && policy.foldCompleted) hidden.add(turn.terminal);
  return {hidden,
    expanded:protectedHistory || (turn.running ? policy.expandRunning : policy.expandCompleted)};
}
export function awayFromBottom(container) {
  return container.scrollHeight - container.clientHeight - Math.max(0, container.scrollTop) > 25;
}
/** @param {HTMLElement} container @param {boolean | undefined} readingSummary @param {HTMLElement} [heading] */
export function readingPosition(container, readingSummary, heading) {
  if (heading) return {atEnd:false,anchors:[{node:heading,offset:heading.getBoundingClientRect().top-container.getBoundingClientRect().top}]};
  const atEnd=!readingSummary && !awayFromBottom(container);
  const anchors=atEnd ? [] : readingAnchor(container);
  return {atEnd,anchors};
}
function readingAnchor(container) {
  const {top,bottom}=container.getBoundingClientRect();
  const rowParent = container.firstElementChild?.classList.contains('transcript-flow')
    ? container.firstElementChild : container;
  const rows=rowParent.querySelectorAll(':scope > :not([hidden])');
  let low=0,high=rows.length;
  while(low<high) {
    const mid=Math.floor((low+high)/2);
    if(rows[mid].getBoundingClientRect().bottom<=top)low=mid+1;
    else high=mid;
  }
  const anchors=[];
  for(let i=low;i<rows.length;i++) {
    const node=rows[i];
    const rect=node.getBoundingClientRect();
    anchors.push({node,offset:rect.top-top});
    if(rect.top>=bottom)break;
  }
  return anchors;
}
export function restoreAnchor(container, anchors) {
  const anchor=anchors.find(item=>item.node.isConnected && !item.node.hidden && item.node.getClientRects().length);
  if(anchor){
    const delta=anchor.node.getBoundingClientRect().top-container.getBoundingClientRect().top-anchor.offset;
    if(delta)container.scrollTop+=delta;
  }
}
