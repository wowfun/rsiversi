// Routing only; Desktop validates native authority at its request boundary.
/** @param {{protocol: string, host: string}} location */
export function isNativeDocument(location = globalThis.location) {
  return (location.protocol === 'rsi:' && location.host === 'localhost') ||
    (location.protocol === 'http:' && location.host === 'rsi.localhost');
}
