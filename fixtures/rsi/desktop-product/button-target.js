const label = arguments[0];
for (const button of document.querySelectorAll('button, summary')) {
  if (label === 'Send' ? button.dataset.testid !== 'composer-send' : (button.getAttribute('aria-label') || button.textContent.trim()) !== label) continue;
  if (button.matches(':disabled') || button.closest('[inert]') || !button.checkVisibility({checkVisibilityCSS: true})) continue;
  button.scrollIntoView({block: 'center'});
  const rect = button.getBoundingClientRect();
  if (rect.width && rect.height && button.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2))) return button;
}
return null;
