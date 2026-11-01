/**
 * The docked split tree: nested flex runs sized by each split's fractions, with a
 * draggable divider between neighbours. A live divider drag renders from the
 * preview fractions instead of the recorded ones — the gesture only settles one
 * intent when it ends.
 */
import { Fragment } from 'react'
import type { ReactNode } from 'react'
import clsx from 'clsx'
import type { LayoutState, NodeId, SplitId, SplitAxis } from '../contract/types.ts'
import { getNode } from '../engine/tree.ts'
import type { PaneCallbacks } from './render.ts'
import { TabPanel } from './TabPanel.tsx'
import css from './dockkit.module.css'

/** Fractions a live divider drag is previewing for one split. */
export interface SizePreview {
  readonly splitId: SplitId
  readonly sizes: readonly number[]
}

/** One subtree of the docked layout. */
export interface PaneTreeProps {
  readonly state: LayoutState
  readonly nodeId: NodeId
  readonly callbacks: PaneCallbacks
  readonly preview: SizePreview | undefined
}

/** Accessible divider shared by nested and stable-tab layouts. */
export function SplitDivider({ id, axis, sizes, index, callbacks, className, style }: {
  id: SplitId; axis: SplitAxis; sizes: readonly number[]; index: number; callbacks: PaneCallbacks
  className?: string; style?: import('react').CSSProperties
}): ReactNode {
  const floor = Math.min(callbacks.minimumPaneFraction, 1 / sizes.length)
  return <div className={clsx(css.divider, className)} style={style}
    data-dockkit-divider={`${id}:${index}`} role="separator" tabIndex={0}
    aria-label={callbacks.labels.resizePane} aria-orientation={axis === 'row' ? 'vertical' : 'horizontal'}
    aria-valuemin={Math.round(floor * 100)}
    aria-valuemax={Math.round(((sizes[index] ?? 0) + (sizes[index + 1] ?? 0) - floor) * 100)}
    aria-valuenow={Math.round((sizes[index] ?? 0) * 100)}
    onKeyDown={event => callbacks.onDividerKeyDown(id, index, event)}
    onPointerDown={event => callbacks.onDividerPressed(id, index, event)} />
}

/** Render a split or pane node and everything under it. */
export function PaneTree({ state, nodeId, callbacks, preview }: PaneTreeProps): ReactNode {
  const node = getNode(state, nodeId)
  if (node.kind === 'pane') return <TabPanel state={state} pane={node} callbacks={callbacks} />
  const sizes = preview !== undefined && preview.splitId === node.id ? preview.sizes : node.sizes
  return (
    <div
      className={clsx(css.split, node.axis === 'row' ? css.splitRow : css.splitColumn)}
      data-dockkit-split={node.id}
    >
      {node.children.map((childId, index) => (
        <Fragment key={childId}>
          {index > 0 && (
            <SplitDivider id={node.id} axis={node.axis} sizes={sizes} index={index - 1} callbacks={callbacks} />
          )}
          <div
            className={css.splitCell}
            data-dockkit-cell={`${node.id}:${index}`}
            style={{ flexGrow: sizes[index] }}
          >
            <PaneTree state={state} nodeId={childId} callbacks={callbacks} preview={preview} />
          </div>
        </Fragment>
      ))}
    </div>
  )
}
