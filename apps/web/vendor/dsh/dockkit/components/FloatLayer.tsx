/**
 * The floating layer: one overlay panel per floating pane, bottom-to-top in the
 * model's z order. A floating pane hosts exactly one tab; its header is the
 * strip's row holding that tab's chip, never selectable or closable from the
 * chip, and the send-back and close controls. Pressing a panel's body raises it. Its grip
 * and corner report through their gesture instead: a press released in place is
 * a click and raises the panel; a drag records the move or resize, and that
 * operation raises the panel itself, so one gesture is one intent. Raising a
 * panel that is active and on top already changes nothing and reports nothing.
 *
 * The layer owns its own drag and resize gestures, so where it mounts is not
 * part of its contract: panels are positioned in viewport coordinates and read
 * only `state` and the outward contracts. An embedder may portal it anywhere,
 * and nothing here assumes the docked tree is an ancestor or even present.
 */
import { useState } from 'react'
import type { PointerEvent as ReactPointerEvent, ReactNode } from 'react'
import clsx from 'clsx'
import { IconCloseOutlineRegular, IconPanelLeftOutlineRegular, Tooltip } from '../../dockkit-primitives/exports.ts'
import type { DockIntents, DockLabels, TabRenderer } from '../contract/adapter.ts'
import type { FloatRect, LayoutState, PaneId, TabId, TabRecord } from '../contract/types.ts'
import { FLOAT_MIN_SIZE } from '../engine/constraints.ts'
import { movedRect, resizedRect } from '../engine/geometry.ts'
import { floatRect, getPane, getTab, onlyTabId } from '../engine/tree.ts'
import { useGesture } from './pointer.ts'
import { TabTitle } from './TabTitle.tsx'
import css from './dockkit.module.css'

/** The layout whose `floats` this layer draws. */
export interface FloatLayerProps {
  readonly state: LayoutState
  readonly intents: DockIntents
  readonly labels: DockLabels
  readonly renderTab: TabRenderer
  /** Whether a floating tab offers its close control; defaults to true. Called per tab on every render. */
  readonly canCloseTab?: (tabId: TabId) => boolean
  /** The panel header's title content; omit to show the record's `title` text (see `DockSurfaceProps`). */
  readonly renderTabTitle?: TabRenderer
}

/** A floating-panel gesture: what it moves and where it started. */
interface FloatDrag {
  readonly mode: 'move' | 'resize'
  readonly originX: number
  readonly originY: number
  readonly rect: FloatRect
}

/** The rectangle a gesture has reached. */
function draggedRect(drag: FloatDrag, x: number, y: number): FloatRect {
  const dx = x - drag.originX
  const dy = y - drag.originY
  return drag.mode === 'move'
    ? movedRect(drag.rect, dx, dy)
    : resizedRect(drag.rect, dx, dy, FLOAT_MIN_SIZE)
}

/** Whether two rectangles agree in every coordinate. */
function sameRect(a: FloatRect, b: FloatRect): boolean {
  return a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height
}

/** Whether a floating pane is already where a raise would put it: focused and on top. */
function raised(state: LayoutState, paneId: PaneId): boolean {
  return state.activePaneId === paneId && state.floats.at(-1) === paneId
}

/**
 * Shared move/resize gestures; changing layout mode never owns the content DOM.
 * @param state - committed layout and float stacking order.
 * @param intents - gesture settlements.
 * @returns the current preview and focus/gesture callbacks.
 */
export function useFloatGestures(state: LayoutState, intents: DockIntents) {
  const [preview, setPreview] = useState<{ paneId: PaneId; rect: FloatRect } | undefined>(undefined)
  const begin = useGesture(() => { setPreview(undefined) })

  /** Focus and raise a panel from a press or click on it, unless it is raised already. */
  const raise = (paneId: PaneId): void => {
    if (raised(state, paneId)) return
    intents.focusPane(paneId)
  }

  /** Start a move or resize from a press on the panel's grip or corner; a release that moved nothing is a click. */
  const drag = (mode: 'move' | 'resize', paneId: PaneId, event: ReactPointerEvent<HTMLElement>): void => {
    // The press stops here: the panel's own press-to-focus would record a focus
    // entry before the drag's, and the release below decides which one it is.
    event.stopPropagation()
    const root = document.documentElement
    const windows = root.hasAttribute('data-windows-titlebar')
    // Desktop publishes the caption height in pixels on the root's inline style.
    const caption = windows && !root.hasAttribute('data-fullscreen')
      ? Number.parseFloat(root.style.getPropertyValue('--dsh-windows-titlebar-height')) : 0
    const saved = floatRect(getPane(state, paneId))
    const rect = { ...saved, y: Math.max(saved.y, windows ? caption + 20 : 0) }
    const start: FloatDrag = { mode, originX: event.clientX, originY: event.clientY, rect }
    begin(event.currentTarget, event.pointerId, {
      move: (moved) => { setPreview({ paneId, rect: draggedRect(start, moved.clientX, moved.clientY) }) },
      up: (released) => {
        const rect = draggedRect(start, released.clientX, released.clientY)
        if (sameRect(rect, start.rect)) raise(paneId)
        else if (mode === 'move') intents.moveFloat(paneId, rect.x, rect.y)
        else intents.resizeFloat(paneId, rect)
      },
    })
  }

  const keyboard = (mode: 'move' | 'resize', paneId: PaneId, event: import('react').KeyboardEvent<HTMLElement>): void => {
    const step = event.shiftKey ? 50 : 10
    const dx = event.key === 'ArrowLeft' ? -step : event.key === 'ArrowRight' ? step : 0
    const dy = event.key === 'ArrowUp' ? -step : event.key === 'ArrowDown' ? step : 0
    if (!dx && !dy && event.key !== 'Home' && event.key !== 'End') return
    event.preventDefault()
    event.stopPropagation()
    const rect = floatRect(getPane(state, paneId))
    const root = document.documentElement
    const caption = root.hasAttribute('data-windows-titlebar') && !root.hasAttribute('data-fullscreen')
      ? Number.parseFloat(root.style.getPropertyValue('--dsh-windows-titlebar-height')) || 0 : 0
    const top = root.hasAttribute('data-windows-titlebar') ? caption + 20 : 0
    const width = root.clientWidth, height = root.clientHeight
    const next = mode === 'move' ? movedRect(rect, dx, dy) : resizedRect(rect, dx, dy, FLOAT_MIN_SIZE)
    if (mode === 'move') {
      const x = event.key === 'Home' ? 0 : event.key === 'End' ? width - rect.width : next.x
      const y = event.key === 'Home' ? top : event.key === 'End' ? height - rect.height : next.y
      intents.moveFloat(paneId, Math.max(0, Math.min(width - rect.width, x)), Math.max(top, Math.min(height - rect.height, y)))
    } else {
      const w = event.key === 'Home' ? FLOAT_MIN_SIZE.width : event.key === 'End' ? width - rect.x : next.width
      const h = event.key === 'Home' ? FLOAT_MIN_SIZE.height : event.key === 'End' ? height - rect.y : next.height
      intents.resizeFloat(paneId, { ...rect,
        width: Math.max(FLOAT_MIN_SIZE.width, Math.min(w, width - rect.x)),
        height: Math.max(FLOAT_MIN_SIZE.height, Math.min(h, height - rect.y)) })
    }
  }

  return { preview, raise, drag, keyboard }
}

/** The same two-axis keyboard and pointer resize grip in both layouts. */
export function FloatResize({ paneId, rect, labels, gestures }: {
  paneId: PaneId; rect: FloatRect; labels: DockLabels; gestures: ReturnType<typeof useFloatGestures>
}): ReactNode {
  return <button type="button" className={css.floatResize} data-dockkit-float-resize={paneId}
    aria-label={labels.resizeFloat} aria-description={labels.describeFloatSize(Math.round(rect.width), Math.round(rect.height))}
    onKeyDown={event => gestures.keyboard('resize', paneId, event)}
    onPointerDown={event => gestures.drag('resize', paneId, event)} />
}

/**
 * Header-only controls, usable above either retained or visibility-mounted bodies.
 * @param props - owning pane/tab, localized labels and gesture callbacks.
 * @returns the float header without a content container.
 */
export function FloatHeader({ paneId, tab, labels, intents, renderTabTitle, canCloseTab, drag, keyboard, rect }: {
  readonly paneId: PaneId
  readonly tab: TabRecord
  readonly labels: DockLabels
  readonly intents: DockIntents
  readonly renderTabTitle: TabRenderer | undefined
  readonly canCloseTab: ((id: TabId) => boolean) | undefined
  readonly drag: ReturnType<typeof useFloatGestures>['drag']
  readonly keyboard: ReturnType<typeof useFloatGestures>['keyboard']
  readonly rect: FloatRect
}): ReactNode {
  return (
    <header className={clsx(css.tabStrip, css.floatHeader)} data-dockkit-float-grip={paneId}
      onPointerDown={(event) => { drag('move', paneId, event) }}>
      <button type="button" className={clsx(css.tab, css.floatTitle)} data-dockkit-float-title
        aria-label={`${labels.moveFloat}: ${tab.title}`} aria-description={labels.describeFloatPosition(Math.round(rect.x), Math.round(rect.y))}
        onKeyDown={event => keyboard('move', paneId, event)}>
        <TabTitle>{renderTabTitle?.(tab) ?? tab.title}</TabTitle>
      </button>
      <div className={css.stripFill} />
      <Tooltip label={labels.dockFloat} side="bottom" delayMs={500}>
        <button type="button" className={css.iconButton} aria-label={labels.dockFloat}
          data-dockkit-float-dock={paneId} onPointerDown={(event) => { event.stopPropagation() }}
          onClick={() => { intents.unfloatPane(paneId) }}>
          <IconPanelLeftOutlineRegular className={css.dockGlyph} />
        </button>
      </Tooltip>
      {(canCloseTab?.(tab.id) ?? true) && (
        <Tooltip label={labels.closeTab} shortcutKeys={labels.closeTabKeys} side="bottom" delayMs={500}>
          <button type="button" className={css.iconButton} aria-label={labels.closeTab}
            data-dockkit-float-close={paneId} onPointerDown={(event) => { event.stopPropagation() }}
            onClick={() => { intents.closeTab(tab.id) }}>
            <IconCloseOutlineRegular />
          </button>
        </Tooltip>
      )}
    </header>
  )
}

/** Every floating panel, in z order. */
export function FloatLayer({ state, intents, labels, renderTab, renderTabTitle, canCloseTab }: FloatLayerProps): ReactNode {
  const gestures = useFloatGestures(state, intents)
  const { preview, raise, drag, keyboard } = gestures

  return (
    <>
      {state.floats.map((paneId, depth) => {
        const pane = getPane(state, paneId)
        const tab = getTab(state, onlyTabId(pane))
        // A panel mid-gesture draws where the pointer has taken it and on top,
        // as the operation its release records will leave it.
        const lifted = preview?.paneId === paneId ? preview.rect : undefined
        const live = lifted ?? floatRect(pane)
        return (
          <div
            key={paneId}
            className={css.float}
            data-dockkit-float={paneId}
            tabIndex={-1}
            data-dockkit-float-active={state.activePaneId === paneId || undefined}
            style={{
              left: live.x,
              top: `max(var(--dsh-dockkit-float-top, 0px), ${live.y}px)`,
              width: live.width,
              height: live.height,
              zIndex: lifted === undefined ? depth + 1 : state.floats.length + 1,
            }}
            onPointerDown={() => { raise(paneId) }}
          >
            <FloatHeader {...{ paneId, tab, labels, intents, renderTabTitle, canCloseTab, drag, keyboard }} rect={live} />
            <div className={css.floatBody}>{renderTab(tab)}</div>
            <FloatResize paneId={paneId} rect={live} labels={labels} gestures={gestures} />
          </div>
        )
      })}
    </>
  )
}
