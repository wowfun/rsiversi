import {DeviceStore} from './device-store.js';
export {defaultLayout,validateLayout,workspacePreference} from './presentation-layout.js';
export function presentationKey(endpoint, principal) {
  if (typeof endpoint !== 'string' || !endpoint || endpoint.length > 1024) throw new Error('Invalid presentation endpoint');
  const owner = principal?.kind === 'local' ? 'local' : principal?.kind === 'device' && typeof principal.device_id === 'string' && principal.device_id.length <= 256 ? `device:${principal.device_id}` : undefined;
  if (!owner) throw new Error('Invalid presentation principal');
  return JSON.stringify([endpoint,owner]);
}

export class PresentationStore {
  constructor(owner) { this.owner = owner; }
  static async open(key) { return new PresentationStore(await DeviceStore.open(key)); }
  async load() { return (await this.owner.read('layouts')).value; }
  async apply(intent) { return (await this.owner.apply('layouts', intent)).value; }
  subscribe(callback) { return this.owner.subscribe('layouts', '', callback); }
  close() { this.owner.close(); }
}
