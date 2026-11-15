export async function createExtension() {
  let aborts = 0;
  return { close() {}, async handle(method) {
    if (method === 'agent.load.begin') return 'transfer';
    if (method === 'agent.load.finish') throw Error('invalid checkpoint');
    if (method === 'agent.load.abort') { aborts++; return new Promise(() => {}); }
    if (method === 'fixture.aborts') return aborts;
    throw Error(`unexpected method: ${method}`);
  } };
}
