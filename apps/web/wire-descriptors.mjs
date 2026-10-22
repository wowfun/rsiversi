import {wireDescriptor} from '../devtools/wire-descriptors.mjs';

export function desktopBridgeDescriptor() {
  return wireDescriptor(new URL('../../crates/rsi/desktop-bridge/wire-schema.json', import.meta.url), 'rsi.desktop.bridge');
}
