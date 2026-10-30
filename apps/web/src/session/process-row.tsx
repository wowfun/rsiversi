import {
  IconBrowseOutlineRegular, IconCodeOutlineRegular, IconApiOutlineRegular, IconEditOutlineRegular,
  IconSearchOutlineRegular, IconThinkOutlineRegular, IconChevronRightOutlineRegular,
} from '../../vendor/dsh/dockkit-primitives/icons/index.tsx';
import type {TranscriptBlock} from './transcript.tsx';

function toolPresentation(name: string) {
  switch (name) {
    case 'file_read': return {label: 'Read', Icon: IconBrowseOutlineRegular};
    case 'directory_list': return {label: 'List', Icon: IconBrowseOutlineRegular};
    case 'bash': return {label: 'Bash', Icon: IconApiOutlineRegular};
    case 'apply_patch': return {label: 'Patch', Icon: IconEditOutlineRegular};
    case 'file_write': return {label: 'Write', Icon: IconEditOutlineRegular};
    case 'file_edit': return {label: 'Edit', Icon: IconEditOutlineRegular};
    case 'file_search': return {label: 'Search', Icon: IconSearchOutlineRegular};
    case 'skill_read': return {label: 'Skill', Icon: IconBrowseOutlineRegular};
    case 'todo_write': return {label: 'To-dos', Icon: IconEditOutlineRegular};
    case 'plan_write': return {label: 'Plan', Icon: IconEditOutlineRegular};
    case 'history_search': return {label: 'Search history', Icon: IconSearchOutlineRegular};
    case 'reference_read': return {label: 'Read reference', Icon: IconBrowseOutlineRegular};
    case 'present': return {label: 'Present', Icon: IconBrowseOutlineRegular};
    default: return {label: name, Icon: IconCodeOutlineRegular};
  }
}

export function ProcessTitle({block, open, toggle}: {block: TranscriptBlock; open: boolean; toggle(): void}) {
  const reasoning = block.role === 'reasoning';
  const {label, Icon} = reasoning ? {label: 'Think', Icon: IconThinkOutlineRegular}
    : toolPresentation(block.tool?.name ?? block.title);
  const preview = reasoning ? block.text.split(/\r?\n/).find(line=>line.trim())?.replaceAll('**','')
    : block.tool?.argument_summary;
  const phase = block.tool?.phase;
  const failure = phase === 'rejected' ? 'Rejected'
    : typeof phase === 'object' && phase.settled !== 'completed' ? 'Failed' : '';
  return <button type="button" className="message-title summary-toggle" aria-expanded={open}
    title={block.title} onClick={toggle}>
    <span className="process-leading" aria-hidden="true"><Icon size={14}/><IconChevronRightOutlineRegular className="process-chevron" size={14}/></span>
    <span className="process-label">{label}</span>
    {!!preview && <><span className="process-separator" aria-hidden="true"/><span className="process-preview">{preview}</span></>}
    {failure && <span className="process-failure">{failure}</span>}
  </button>;
}

export function RunningStatus() {
  return <div className="turn-running" role="status"><IconThinkOutlineRegular size={14}/><span>Working…</span></div>;
}
