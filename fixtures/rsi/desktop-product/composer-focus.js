const input = document.querySelector('textarea[aria-label="Main message"]');
const style = getComputedStyle(input);
return {
  focused: document.activeElement === input,
  focusVisible: input.matches(':focus-visible'),
  outlineWidth: parseFloat(style.outlineWidth),
  outlineStyle: style.outlineStyle,
  outlineColor: style.outlineColor,
  forcedColors: matchMedia('(forced-colors:active)').matches,
  inputEventsBefore: arguments[0],
  inputEventsAfter: window.fixtureInputEvents.length,
  trustedInput: window.fixtureInputEvents.slice(arguments[0]).some(event => event.trusted && event.length === input.value.length),
};
