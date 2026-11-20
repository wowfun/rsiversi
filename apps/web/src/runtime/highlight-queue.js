import {createHighlightSession} from '../../code-highlight.js';

function afterFrame(run) {
  let timer;
  const frame = requestAnimationFrame(() => { timer = setTimeout(run, 0); });
  return () => { cancelAnimationFrame(frame); clearTimeout(timer); };
}

// Optional presentation work shares one bundle-owned queue, never the frame ACK lane.
// Cancellation removes retained source; callers keep exact plain text on overflow.
export function createHighlightQueue({createSession = createHighlightSession, schedule = afterFrame, now = () => performance.now()} = {}) {
  const jobs = new Set();
  let units = 0, retainedTokens = 0, cancelScheduled;
  function release(job) {
    if(!jobs.delete(job))return;
    units-=job.text.length;retainedTokens-=job.retained;
    job.session=null;
  }
  function wake() {
    if (cancelScheduled || !jobs.size) return;
    cancelScheduled = schedule(() => {
      cancelScheduled = undefined;
      const job = jobs.values().next().value;
      if (!job) return;
      let result={status:'plain'}, before=0;
      const started=now();
      try {
        job.session??=createSession(job.text,job.language);
        before=job.session?.progress??0;
        result=job.session?.step(Math.min(16,500-job.spent))??result;
      } catch { /* Optional grammar work must not fail an accepted presentation. */ }
      job.spent+=now()-started;
      if(job.cancelled){wake();return;}
      retainedTokens+=(job.session?.retainedTokens??0)-job.retained;
      job.retained=job.session?.retainedTokens??0;
      const pending=result.status==='pending' && job.session.progress>before;
      if(retainedTokens>32768||job.spent>=500)result={status:'plain'};
      if(result.status==='pending'&&pending){jobs.delete(job);jobs.add(job);}
      else {
        release(job);
        job.complete(result.status==='complete'?result.tokens:null);
      }
      wake();
    });
  }
  return {
    enqueue(text, language, complete) {
      if (jobs.size >= 128 || text.length > 256 * 1024 || units + text.length > 1024 * 1024)
        return () => {};
      const job = {text, language, complete, session:null, retained:0, spent:0, cancelled:false};
      jobs.add(job);units+=text.length;wake();
      return () => {
        job.cancelled=true;release(job);
        if(!jobs.size){cancelScheduled?.();cancelScheduled=undefined;}
      };
    },
  };
}

export const highlights = createHighlightQueue();
