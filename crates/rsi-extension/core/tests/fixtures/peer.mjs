import { createInterface } from 'node:readline';
const generation = process.argv[2];
const send = message => process.stdout.write(JSON.stringify({ generation, message }) + '\n');
const held = new Set();
createInterface({ input: process.stdin }).on('line', line => {
  const { message } = JSON.parse(line);
  if (message.kind === 'cancel' && held.delete(message.id)) {
    send({ kind: 'reply', id: message.id, result: { status: 'error', value: 'cancelled' } });
  } else if (message.kind === 'request') {
    if (message.method === 'hold') { held.add(message.id); return; }
    const value = message.method === 'extension.hello' ? { protocol: 1 } : message.payload;
    send({ kind: 'reply', id: message.id, result: { status: 'ok', value } });
  }
});
