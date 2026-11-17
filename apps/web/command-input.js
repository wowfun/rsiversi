// Completion replaces its trigger; menu selection keeps the complete draft.
export function commandSelection(text, start, end, command, trigger = false) {
  if (!command.composer_input) throw new Error('Use this command through its dedicated controls');
  if (command.composer_input.kind === 'none') return {kind:'execute',consume:trigger && start < end ? {start,end} : undefined};
  if (trigger && text.slice(0,start).trim()) throw new Error('Move the cursor to the start of the message to enter command arguments');
  if (!trigger && commandToken(text)?.[1] === command.name) return {kind:'edit',text};
  const tail = trigger ? text.slice(end) : text;
  return {kind:'edit',text:`/${command.name}${/^\s/.test(tail) ? '' : ' '}${tail}`};
}

export const commandToken = text => /^\s*\/([a-zA-Z0-9_.-]+)(?:\s|$)/.exec(text);

export function claimedCommand(text, commands) {
  const match = commandToken(text);
  if (!match) return;
  if (match[1] === 'export') return {name:'export',hint:'[PATH] [-f markdown|md|json] [-i includes]',end:match[0].length};
  const command = commands?.find(command => command.name === match[1] && command.composer_input?.kind === 'text');
  return command && {name:command.name,hint:command.composer_input.hint,end:match[0].length};
}
