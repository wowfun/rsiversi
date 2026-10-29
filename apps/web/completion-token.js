// Preserve the document's literal backtick suppression grammar, without split arrays.
export function completionToken(text, cursor) {
  const before = text.slice(0, cursor), match = /(?:^|[\s([{])(@[A-Za-z0-9_.-]*)$/.exec(before);
  if (match) {
    const start = cursor - match[1].length, suffix = /^[A-Za-z0-9_.-]*/.exec(text.slice(cursor))[0];
    const end = cursor + suffix.length, name = text.slice(start + 1, end), trailing = text[end];
    const locator = trailing === '/' || trailing === ':' && (name === 'path_hex' || end + 1 < text.length && !/\s/u.test(text[end + 1]));
    if ((!name || /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/.test(name)) && !locator) {
      let ticks = 0, triples = 0, run = 0;
      for (let i = 0; i < cursor; i++) {
        if (text[i] === '`') { ticks ^= 1; if (++run === 3) { triples ^= 1; run = 0; } }
        else run = 0;
      }
      if (!ticks && !triples) return { query: text.slice(start, end), start, end, text, cursor, agent: true };
    }
  }
  if (!text.startsWith('/') || text.startsWith('//') || /[\r\n]/.test(text)) return;
  const end = text.search(/\s/), stop = end < 0 ? text.length : end;
  if (cursor > stop) return;
  return { query: text.slice(1, stop), start: 0, end: stop, text, cursor, agent: false };
}
