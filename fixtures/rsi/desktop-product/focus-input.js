const selector = arguments[0], label = selector.match(/aria-label="([^"]+)"/)?.[1];
const input = document.querySelector(selector) || [...document.querySelectorAll('label')].find(e => e.firstChild?.textContent.trim() === label)?.control;
if (!input || input.matches(':disabled') || input.readOnly) return null;
input.scrollIntoView({block: 'center'});
const rect = input.getBoundingClientRect();
if (!rect.width || !rect.height || !input.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2))) return null;
input.focus({preventScroll: true});
return document.activeElement === input ? input : null;
