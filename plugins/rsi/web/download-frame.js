// The controlled frame has no Session authority. Its identity is attested by
// the owning page over that page's private MessagePort before download begins.
const id = location.pathname.slice('/downloads/'.length);
if (/^[a-f0-9]{64}$/.test(id) && parent !== window && navigator.serviceWorker.controller) {
  const channel = new MessageChannel();
  channel.port1.onmessage = ({data}) => {
    channel.port1.close();
    if (data?.kind === 'identity' && typeof data.client === 'string') {
      parent.postMessage({kind:'rsi-export-frame',id,client:data.client},location.origin);
    }
  };
  navigator.serviceWorker.controller.postMessage({kind:'identify',id},[channel.port2]);
  window.addEventListener('message',event => {
    if (event.origin !== location.origin || event.source !== parent || event.data?.kind !== 'rsi-export-download' || event.data.id !== id || !/^[a-f0-9]{64}$/.test(event.data.nonce)) return;
    location.href = `/downloads/${id}/file/${event.data.nonce}`;
  },{once:true});
}
