// Opt-in fixture instrumentation; no protocol or agent implementation is replaced.
import {appendFileSync} from 'node:fs';
appendFileSync(process.env.RSI_DSH_PEER_PIDS,`${process.pid}\n`);
