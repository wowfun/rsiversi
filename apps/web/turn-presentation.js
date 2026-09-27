// Display-only adapter. Pane still owns drafts, submission and attachment lifetime.
export function turnVisibility(turn, mode, expanded) {
  const protectedHistory = !turn.foldable;
  const collapse = !protectedHistory && mode !== 'verbose' && !expanded && (!turn.running || mode === 'compact');
  const candidate = new Set(turn.candidate);
  return {hidden:new Set(collapse ? turn.process.filter(key=>!candidate.has(key)) : []),
    expanded:protectedHistory || mode==='verbose' || mode==='detailed' || expanded};
}
export class TurnPresentation {
  constructor(container, redraw) { this.container=container; this.redraw=redraw; this.rows=new Map(); this.expanded=new Map(); }
  reset() { for(const row of this.rows.values())row.remove();this.rows.clear();this.expanded.clear(); }
  apply(index, blocks, mode) {
    const turns=index?.entries??{};
    for(const [id,row] of this.rows) if(!Object.hasOwn(turns,id)){row.remove();this.rows.delete(id);this.expanded.delete(id);}
    const desired=new Map();
    for(const turn of Object.values(turns)) {
      const visibility=turnVisibility(turn,mode,this.expanded.get(turn.id));
      for(const key of turn.blocks) desired.set(key,{hidden:visibility.hidden.has(key),expanded:!!visibility.expanded});
      const first=turn.process.map(key=>blocks.get(key)?.node).find(Boolean);
      let row=this.rows.get(turn.id);
      if(!first || mode==='verbose'){row?.remove();continue;}
      if(!row){row=document.createElement('button');row.type='button';row.className='turn-summary';row.dataset.turnId=turn.id;row.addEventListener('click',()=>{this.expanded.set(turn.id,!this.expanded.get(turn.id));this.redraw();});this.rows.set(turn.id,row);}
      const label=`${visibility.hidden.size?'▸':'▾'} ${turn.running?'Working':turn.status} · ${turn.process.length} ${turn.process.length===1?'step':'steps'}${turn.partial?' · Partial history':''}`;
      if(row.textContent!==label)row.textContent=label;
      const expanded=String(!visibility.hidden.size);
      if(row.getAttribute('aria-expanded')!==expanded)row.setAttribute('aria-expanded',expanded);
      if(row.nextSibling!==first)first.before(row);
    }
    for(const [key,{node}] of blocks) {
      const state=desired.get(key);
      const hidden=!!state?.hidden && !node.classList.contains('delegation');
      if(node.hidden!==hidden)node.hidden=hidden;
      const expanded=!!state?.expanded;
      if(node.classList.contains('turn-expanded')!==expanded)node.classList.toggle('turn-expanded',expanded);
    }
  }
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
