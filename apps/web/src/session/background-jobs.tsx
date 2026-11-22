import {useEffect, useMemo, useState} from 'react';
import {IconChevronDownOutlineRegular, IconStopFillRegular} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import {Tooltip} from '../../vendor/dsh/dockkit-primitives/Tooltip.tsx';
import {StateDot, type StateDotState} from '../../vendor/dsh/primitives/StateDot.tsx';
import {TerminalBlock, type TerminalBlockLabels} from '../../vendor/dsh/primitives/TerminalBlock.tsx';
import css from '../../vendor/dsh/jobs/JobsSection.module.css';
import {input, type BackgroundOutput} from '../bridge.ts';
import type {BackgroundJob} from '../presentation-types.ts';

const colors: Record<BackgroundJob['status'], StateDotState> = {
  running:'ongoing', stopping:'warning', completed:'done', failed:'error', cancelled:'warning', outcome_unknown:'warning',
};
const terminalLabels:TerminalBlockLabels = {
  commandLine:n => `Command line ${n}`, signal:s => `signal ${s}`, exitCode:n => `exit ${n}`,
  noExitCode:'no exit code', running:'running', failed:'failed', done:'done', copy:'Copy', copied:'Copied',
  noOutput:'(no output)', collapseAria:'Collapse output', collapse:'Collapse',
  expandAria:n => `Expand ${n} collapsed output lines`, expand:n => `Show ${n} more lines`,
};
export function liveJob(job:BackgroundJob) { return job.status === 'running' || job.status === 'stopping'; }
function elapsed(ms:number) {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  const minutes = Math.floor(seconds / 60);
  if (minutes >= 60) return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
  return minutes ? `${minutes}m ${seconds % 60}s` : `${seconds}s`;
}
const identity = (job:BackgroundJob) => job.origin ?? job.id;
type Binding = {pane:string; generation:string};

function Output({pane, generation, job}:Binding & {job:BackgroundJob}) {
  const [output, setOutput] = useState<BackgroundOutput|null>(null);
  const [error, setError] = useState('');
  const live = liveJob(job);
  const {id, origin} = job;
  useEffect(() => input.observeBackgroundJob(pane, generation, {id,origin}, value => {
    setOutput(value);
    setError('');
  }, setError), [pane, generation, id, origin]);
  return <div className={css.panel}>
    {output?.truncated && <div className={css.notice}>… earlier output dropped …</div>}
    {error && <div role="status" className={css.noticeError}>live output stream interrupted: {error}</div>}
    <TerminalBlock command={job.name} output={output ? output.stdout + output.stderr : ''} running={live}
      copyText={job.name} runStateDot={false} maxLines={Infinity} labels={terminalLabels}/>
  </div>;
}

export function useJobFolds() {
  const [cleared, setCleared] = useState<ReadonlySet<string>>(() => new Set());
  const [finishedOpen, setFinishedOpen] = useState<boolean>();
  const [more, setMore] = useState({running:false, finished:false});
  return {cleared, setCleared, finishedOpen, setFinishedOpen, more, setMore};
}

export function BackgroundJobs({pane, generation, rows, folds}:Binding & {rows:BackgroundJob[]; folds:ReturnType<typeof useJobFolds>}) {
  const [expanded, setExpanded] = useState<string>();
  const [stop, setStop] = useState<{key:string; phase:'armed'|'pending'|'failed'}>();
  const [owner, setOwner] = useState(false);
  const [now, setNow] = useState(Date.now);
  const running = useMemo(() => rows.filter(liveJob).sort((a,b) => b.started_ms-a.started_ms), [rows]);
  const finished = useMemo(() => rows.filter(job => !liveJob(job) && !folds.cleared.has(identity(job)))
    .sort((a,b) => (b.finished_ms! - a.finished_ms!) || a.started_ms-b.started_ms), [rows, folds.cleared]);
  const hasLive = running.length > 0;
  const finishedOpen = folds.finishedOpen ?? !hasLive;
  useEffect(() => {
    if (!hasLive) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    let mounted = true;
    void input.backgroundJob(pane, generation, {kind:'control'}).then(result => {
      if (mounted) setOwner(result === true);
    }, () => { if (mounted) setOwner(false); });
    return () => { mounted = false; clearInterval(timer); };
  }, [hasLive, pane, generation]);
  useEffect(() => {
    if (!stop || stop.phase === 'pending') return;
    const timer = setTimeout(() => setStop(undefined), stop.phase === 'armed' ? 3000 : 4000);
    return () => clearTimeout(timer);
  }, [stop]);
  function pressStop(job:BackgroundJob) {
    const key = identity(job);
    if (stop?.key !== key || stop.phase !== 'armed') { setStop({key,phase:'armed'}); return; }
    setStop({key,phase:'pending'});
    void input.backgroundJob(pane, generation, {kind:'stop',job:{id:job.id,origin:job.origin}})
      .catch(() => setStop(current => current?.key === key ? {key,phase:'failed'} : current));
  }
  function group(jobs:BackgroundJob[], kind:'running'|'finished') {
    const foldable = jobs.length > 6;
    const unfolded = folds.more[kind];
    const label = unfolded ? 'Show less' : `Show ${jobs.length - 5} more ${kind}`;
    return <>{(foldable && !unfolded ? jobs.slice(0,5) : jobs).map(job => {
      const key = identity(job), isOpen = expanded === key, live = liveJob(job);
      const observable = !!job.origin && (live || job.output_retained);
      const phase = stop?.key === key ? stop.phase : undefined;
      const stopHint = phase === 'armed' ? 'Click again to confirm' : phase === 'failed' ? 'Stop failed' : 'Stop task';
      const status = job.status.replace('_',' ');
      const body = <><span className={css.glyph}><StateDot state={colors[job.status]}/></span>
        <span className={css.label} data-job-label="">{job.name}</span>
        <span className={css.duration}>{elapsed((job.finished_ms ?? now)-job.started_ms)}</span></>;
      return <li key={key} className={css.item}>
        <div className={`${css.rowLine} ${observable ? css.rowInteractive : ''}`} title={status}>
          {observable ? <button className={css.row} type="button" aria-expanded={isOpen}
            aria-label={`${isOpen ? 'Hide' : 'Show'} live output of ${job.name}`} aria-description={status}
            onClick={() => setExpanded(isOpen ? undefined : key)}>{body}</button> : <div className={css.row} aria-label={status}>{body}</div>}
          {owner && job.origin && job.status === 'running' && <Tooltip portal label={stopHint} side="top">
            <button type="button" className={`${css.stop} ${phase === 'armed' ? css.stopArmed : phase === 'failed' ? css.stopFailed : ''}`}
              aria-label={phase === 'armed' || phase === 'failed' ? stopHint : `Stop task ${job.name}`} disabled={phase === 'pending'} onClick={() => pressStop(job)}>
              <IconStopFillRegular size={12}/>{phase === 'armed' && <span className={css.stopLabel}>Confirm stop</span>}
            </button></Tooltip>}
          <span className={css.chevron} aria-hidden>{observable && <IconChevronDownOutlineRegular size={14} className={isOpen ? css.chevronOpen : undefined}/>}</span>
        </div>
        {isOpen && observable && <Output pane={pane} generation={generation} job={job}/>}
      </li>;
    })}{foldable && <li><button className={css.sectionToggle} type="button" aria-expanded={unfolded}
      aria-label={`${label} (${kind} background jobs)`} onClick={() => folds.setMore({...folds.more,[kind]:!unfolded})}>{label}</button></li>}</>;
  }
  if (!hasLive && !finished.length) return null;
  return <section aria-label="Background jobs"><h2>Background jobs</h2><ul className={css.list} data-jobs-section="">
    {group(running, 'running')}
    {finished.length > 0 && <li className={css.sectionHeader}>
      <button type="button" className={css.sectionToggle} aria-expanded={finishedOpen} onClick={() => folds.setFinishedOpen(!finishedOpen)}>
        Finished {finished.length}<IconChevronDownOutlineRegular size={14} className={`${css.sectionChevron} ${finishedOpen ? css.sectionChevronOpen : ''}`}/></button>
      <button type="button" className={css.sectionClear} onClick={() => {
        folds.setCleared(new Set(rows.filter(job => !liveJob(job)).map(identity)));
        folds.setMore({...folds.more,finished:false});
        if (finished.some(job => identity(job) === expanded)) setExpanded(undefined);
      }}>Clear</button></li>}
    {finishedOpen && group(finished, 'finished')}
  </ul></section>;
}
