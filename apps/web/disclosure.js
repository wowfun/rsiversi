const selector='details.composer-extras,details.delivery-menu,details.conversation-menu,details.session-options,details.source-menu:not(.mode-verbose .source-menu)';
export function closeDisclosure(target, restore=false) {
  const details=target?.closest?.(selector);
  if(!details?.open)return false;
  restore ||= details.contains(details.ownerDocument?.activeElement);
  details.open=false;
  if(restore)details.querySelector('summary')?.focus();
  return true;
}
export function installDisclosures(root) {
  const key=event=>{
    if(event.key!=='Escape'||event.isComposing||event.altKey||event.ctrlKey||event.metaKey||event.shiftKey)return;
    const target=event.target.closest?.(selector)??[...root.querySelectorAll(selector)].reverse().find(details=>details.open);
    if(closeDisclosure(target,true)){event.preventDefault();event.stopPropagation();}
  };
  const outside=event=>{for(const details of root.querySelectorAll(selector))if(details.open&&!details.contains(event.target))details.open=false;};
  const focus=event=>{
    const details=event.target.closest?.(selector);
    // Focus leaves the summary before the target button receives its click.
    // activeElement may still be body at the microtask checkpoint between events.
    if(event.relatedTarget&&details?.contains(event.relatedTarget))return;
    queueMicrotask(()=>{if(details?.open&&!details.contains(root.activeElement))details.open=false;});
  };
  const change=event=>{if(event.target.tagName==='SELECT')closeDisclosure(event.target,true);};
  root.addEventListener('keydown',key,true);root.addEventListener('pointerdown',outside,true);
  root.addEventListener('focusout',focus);root.addEventListener('change',change);
  return()=>{root.removeEventListener('keydown',key,true);root.removeEventListener('pointerdown',outside,true);root.removeEventListener('focusout',focus);root.removeEventListener('change',change);};
}
