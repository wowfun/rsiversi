// Only bridge reads are retryable. The Rust owner reconciles the unchanged ACK.
export const backpressure = error => error?.notAdmitted === true && error.retryable !== false;
export async function readTerminalPage(read, ack, active, wait = ms => new Promise(resolve => setTimeout(resolve, ms))) {
  const delays = [100, 250, 500];
  let failures = 0, pressure = 0;
  while (active()) {
    try { return await read(ack); }
    catch (error) {
      if (!active()) return;
      if (backpressure(error)) { await wait(delays[Math.min(pressure++, delays.length - 1)]); continue; }
      if (failures === delays.length) throw error;
      await wait(delays[failures++]);
    }
  }
}
