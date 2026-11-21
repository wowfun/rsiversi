import { ICON_SYMBOLS, type IconSymbolId } from '../vendor/dsh/dockkit-primitives/icons/artwork.ts'

// Only reviewed, statically imported SVG enters this sink. Never pass runtime data.
const usedSymbols = [
  'dsw-icon-mail-read-outline-regular',
  'dsw-icon-mail-unread-outline-regular',
  'dsw-icon-agent-preset-outline-regular',
  'dsw-icon-api-outline-regular',
  'dsw-icon-branch-outline-regular',
  'dsw-icon-context-injection-outline-regular',
  'dsw-icon-think-outline-regular',
  'dsw-icon-code-outline-regular',
  'dsw-icon-browse-outline-regular',
  'dsw-icon-pin-outline-regular',
  'dsw-icon-archive-outline-regular',
  'dsw-icon-archive-check-outline-regular',
  'dsw-icon-archive-off-outline-regular',
  'dsw-icon-chevrons-up-down-outline-regular',
  'dsw-icon-clock-outline-regular',
  'dsw-icon-goal-outline-regular',
  'dsw-icon-paper-plane-outline-regular',
  'dsw-icon-users-outline-regular',
  'dsw-icon-flat-list-outline-regular',
  'dsw-icon-folder-close-regular',
  'dsw-icon-folder-open-regular',
  'dsw-icon-folder-open-outline-regular',
  'dsw-icon-project-add-outline-regular',
  'dsw-icon-queue-outline-regular',
  'dsw-icon-sliders-two-outline-regular',
  'dsw-icon-workspace-tree-outline-regular',

  'dsw-icon-check-outline-regular',
  'dsw-icon-checklist-outline-regular',
  'dsw-icon-check-circle-outline-regular',
  'dsw-icon-chevron-down-outline-regular',
  'dsw-icon-chevron-left-outline-medium',
  'dsw-icon-chevron-left-outline-regular',
  'dsw-icon-chevron-right-outline-regular',
  'dsw-icon-chevron-up-outline-regular',
  'dsw-icon-close-fill-regular',
  'dsw-icon-close-outline-regular',
  'dsw-icon-copy-outline-regular',
  'dsw-icon-database-outline-regular',
  'dsw-icon-gauge-outline-regular',
  'dsw-icon-data-outline-medium',
  'dsw-icon-data-outline-regular',
  'dsw-icon-dark-outline-medium',
  'dsw-icon-dislike-fill-regular',
  'dsw-icon-dislike-outline-regular',
  'dsw-icon-ellipsis-outline-regular',
  'dsw-icon-edit-outline-regular',
  'dsw-icon-followsystem-outline-medium',
  'dsw-icon-light-outline-medium',
  'dsw-icon-like-fill-regular',
  'dsw-icon-like-outline-regular',
  'dsw-icon-new-chat-outline-regular',
  'dsw-icon-nowrap-fill-regular',
  'dsw-icon-panel-left-outline-regular',
  'dsw-icon-personalization-outline-medium',
  'dsw-icon-plugin-pinwheel-outline-medium',
  'dsw-icon-plus-outline-regular',
  'dsw-icon-refresh-outline-regular',
  'dsw-icon-search-outline-regular',
  'dsw-icon-send-outline-medium',
  'dsw-icon-send-outline-regular',
  'dsw-icon-settings-outline-regular',
  'dsw-icon-sliders-two-outline-medium',
  'dsw-icon-trash-outline-regular',
  'dsw-icon-warning-outline-regular',
  'dsw-icon-wrap-fill-regular',
] as const satisfies readonly IconSymbolId[]
const symbols = usedSymbols
  .map(id =>
    ICON_SYMBOLS[id].replace('<svg ', `<symbol id="${id}" `).replace('</svg>', '</symbol>'),
  )
  .join('')

export function IconSprite() {
  return (
    <svg
      className="icon-sprite"
      width="0"
      height="0"
      aria-hidden="true"
      focusable="false"
    >
      <defs dangerouslySetInnerHTML={{ __html: symbols }} />
    </svg>
  )
}
