// Display-only visibility and scroll calculations shared by React presentation and tests.
const policies = {
  compact: {foldCompleted:true, expandRunning:false, expandCompleted:false, liveDetail:false, reasoningPreview:false, foldRunning:true},
  standard: {foldCompleted:true, expandRunning:false, expandCompleted:false, liveDetail:true, reasoningPreview:true, foldRunning:false},
  detailed: {foldCompleted:true, expandRunning:true, expandCompleted:false, liveDetail:true, reasoningPreview:true, foldRunning:false},
  verbose: {foldCompleted:false, expandRunning:true, expandCompleted:true, liveDetail:false, reasoningPreview:true, foldRunning:false},
};
export const presentationPolicy = mode => policies[mode] ?? policies.standard;
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
