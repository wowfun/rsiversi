// Capture before replacement or disabling the active control; never take focus
// from another section. A loading host remains the focus owner between frames.
export function settingsFocus(host) {
  const previous=document.activeElement,owned=host.contains(previous);
  return()=>{
    if(!owned||!host.isConnected)return;
    const available=node=>node!==host&&host.contains(node)&&!node.matches(':disabled')&&node.getClientRects().length&&getComputedStyle(node).visibility!=='hidden';
    if(available(document.activeElement)&&document.activeElement.tabIndex>=0)return;
    if(available(previous)&&previous.tabIndex>=0){previous.focus();return}
    const target=[...host.querySelectorAll('button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled),[tabindex="0"]')]
      .find(node=>node.getClientRects().length&&getComputedStyle(node).visibility!=='hidden');
    if(target)target.focus();
    // Reattaching a retained form can drop focus to body while its controls are
    // disabled. Keep that form as the owner until an enabled control arrives.
    else if(available(document.activeElement))return;
    else if(available(previous))previous.focus();
    else{host.tabIndex=-1;host.focus()}
  };
}
