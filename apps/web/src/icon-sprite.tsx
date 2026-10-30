import { ICON_SYMBOLS, type IconSymbolId } from '../vendor/dsh/dockkit-primitives/icons/artwork.ts'

// Only reviewed, statically imported SVG enters this sink. Never pass runtime data.
const usedSymbols = [
  'dsw-icon-api-outline-regular',
  'dsw-icon-think-outline-regular',
  'dsw-icon-code-outline-regular',
  'dsw-icon-browse-outline-regular',
  'dsw-icon-archive-check-outline-regular',
  'dsw-icon-archive-off-outline-regular',
  'dsw-icon-chevrons-up-down-outline-regular',
  'dsw-icon-clock-outline-regular',
  'dsw-icon-flat-list-outline-regular',
  'dsw-icon-folder-close-regular',
  'dsw-icon-folder-open-outline-regular',
  'dsw-icon-project-add-outline-regular',
  'dsw-icon-queue-outline-regular',
  'dsw-icon-sliders-two-outline-regular',
  'dsw-icon-workspace-tree-outline-regular',

  'dsw-icon-check-outline-regular',
  'dsw-icon-chevron-down-outline-regular',
  'dsw-icon-chevron-left-outline-regular',
  'dsw-icon-chevron-right-outline-regular',
  'dsw-icon-close-fill-regular',
  'dsw-icon-close-outline-regular',
  'dsw-icon-copy-outline-regular',
  'dsw-icon-database-outline-regular',
  'dsw-icon-gauge-outline-regular',
  'dsw-icon-data-outline-medium',
  'dsw-icon-ellipsis-outline-regular',
  'dsw-icon-edit-outline-regular',
  'dsw-icon-new-chat-outline-regular',
  'dsw-icon-nowrap-fill-regular',
  'dsw-icon-panel-left-outline-regular',
  'dsw-icon-personalization-outline-medium',
  'dsw-icon-plugin-pinwheel-outline-medium',
  'dsw-icon-plus-outline-regular',
  'dsw-icon-search-outline-regular',
  'dsw-icon-send-outline-medium',
  'dsw-icon-settings-outline-regular',
  'dsw-icon-sliders-two-outline-medium',
  'dsw-icon-trash-outline-regular',
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
