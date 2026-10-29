import {highlightCode} from '../../code-highlight.js';

function afterFrame(run) {
  let timer;
  const frame = requestAnimationFrame(() => { timer = setTimeout(run, 0); });
  return () => { cancelAnimationFrame(frame); clearTimeout(timer); };
}

// Optional presentation work shares one document queue, never the frame ACK lane.
// Cancellation removes retained source; callers keep exact plain text on overflow.
export function createHighlightQueue({highlight = highlightCode, schedule = afterFrame, now = () => performance.now()} = {}) {
  const jobs = new Set();
  let units = 0, cancelScheduled;
  function wake() {
    if (cancelScheduled || !jobs.size) return;
    cancelScheduled = schedule(() => {
      cancelScheduled = undefined;
      const job = jobs.values().next().value;
      if (!job) return;
      jobs.delete(job); units -= job.text.length;
      let tokens = null, exhausted = false;
      const started = now();
      try { tokens = highlight(job.text, job.language, {budgetMs: Math.min(16, 500 - job.spent), onBudgetExceeded: () => { exhausted = true; }}); }
      catch { /* Optional grammar work must not fail an accepted presentation. */ }
      job.spent += now() - started;
      job.attempts++;
      if (!job.cancelled) {
        if (tokens === null && exhausted && job.spent < 500 && job.attempts < 32) {
          // Lazy grammar compilation can consume several slices. Rejoin the
          // back of the queue so other blocks and input get their turn.
          jobs.add(job); units += job.text.length;
        } else job.complete(tokens);
      }
      wake();
    });
  }
  return {
    enqueue(text, language, complete) {
      if (jobs.size >= 128 || text.length > 256 * 1024 || units + text.length > 1024 * 1024)
        return () => {};
      const job = {text, language, complete, attempts: 0, spent: 0, cancelled: false};
      jobs.add(job); units += text.length; wake();
      return () => {
        job.cancelled = true;
        if (jobs.delete(job)) units -= text.length;
        if (!jobs.size) { cancelScheduled?.(); cancelScheduled = undefined; }
      };
    },
  };
}

export const highlights = createHighlightQueue();
