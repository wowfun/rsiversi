// Isolated product-fixture observations; this never dispatches an action.
export function recordNavigationEvidence() {
  const evidence = window.navigationEvidence = {commands: [], gestures: []};
  const retain = (list, value) => {list.push(value);if(list.length > 64)list.shift();};
  let nextOwner = 0;
  const NativeWorker = window.Worker;
  window.Worker = class extends NativeWorker {
    #owner;
    constructor(...args) {
      super(...args);
      this.#owner = ++nextOwner;
      this.addEventListener('message', ({data}) => {
        if(data?.kind !== 'reply')return;
        const command = evidence.commands.find(value => value.owner === this.#owner && value.id === data.id);
        if(command) {
          command.replied = true;
          command.ok = !data.error;
          if(data.error)command.error = String(data.error).slice(0, 1024);
        }
      });
    }
    postMessage(packet, ...rest) {
      if(packet?.kind === 'call' && packet.method === 'command' && typeof packet.payload === 'string') {
        let value;
        try {value = JSON.parse(packet.payload);} catch {}
        if(['open', 'refresh', 'navigate'].includes(value?.action)) {
          const command = {owner:this.#owner, id:packet.id, action:value.action, replied:false};
          if(value.action === 'open') {command.session = value.session;command.pane = value.pane;}
          if(value.action === 'navigate')command.kind = value.command?.kind;
          retain(evidence.commands, command);
        }
      }
      return super.postMessage(packet, ...rest);
    }
  };
  for(const type of ['mousedown', 'mouseup', 'click'])document.addEventListener(type, event => {
    const button = event.target instanceof Element && event.target.closest('[data-testid="conversation-open"]');
    if(!button)return;
    retain(evidence.gestures, {type, session:button.closest('[data-session-id]')?.dataset.sessionId,
      connected:button.isConnected, selected:document.querySelector('[id^="pane-tab-"][aria-pressed="true"]')?.id});
  }, true);
}
