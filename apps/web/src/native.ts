// Browser and Desktop share the same Worker transport.
export function createDocumentTransport(): Worker {
  return new Worker('/worker.js', {type: 'module'});
}
