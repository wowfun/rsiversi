import { useLayoutEffect, useRef, useState } from 'react'
import { conversationWidth } from '../presentation-layout.js'
import { useLayout } from './presentation.tsx'
import css from './conversation-width.module.css'

export function ConversationWidthControls({
  container,
  showHandles = true,
}: {
  showHandles?: boolean
  container: HTMLElement | null
}) {
  const { layout, update } = useLayout()
  const [column, setColumn] = useState(0),
    [preview, setPreview] = useState<number>()
  const drag = useRef<{
    x: number
    base: number
    latest: number
    side: number
  }>()
  const maximum = Math.max(640, Math.floor(column - 176))
  const bounded = (width: number) =>
    Math.max(640, Math.min(maximum, Math.round(width)))
  const width = conversationWidth(preview ?? layout.conversationWidth, column)
  useLayoutEffect(() => {
    const node = container
    if (!node) return
    const measure = () => setColumn(node.clientWidth)
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    measure()
    return () => observer.disconnect()
  }, [container])
  useLayoutEffect(() => {
    container?.style.setProperty('--conversation-width', `${width}px`)
  }, [container, width])
  const finish = (commit: boolean) => {
    const current = drag.current
    drag.current = undefined
    setPreview(undefined)
    if (commit && current && current.latest !== current.x)
      update({
        conversationWidth: bounded(
          current.base + (current.latest - current.x) * current.side * 2,
        ),
      })
  }
  // At small widths the transcript already occupies its column's usable space.
  if (!showHandles || column < 816 || column - width < 96) return null
  return (
    <div className={css.controls}>
      {(['left', 'right'] as const).map((side) => (
        <div
          key={side}
          className={css.handle}
          data-side={side}
          role="separator"
          tabIndex={0}
          aria-label={`Resize conversation (${side})`}
          aria-orientation="vertical"
          aria-valuemin={640}
          aria-valuemax={Math.max(maximum, width)}
          aria-valuenow={Math.round(width)}
          onKeyDown={(event) => {
            const sign = side === 'right' ? 1 : -1
            if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
              event.preventDefault()
              update({
                conversationWidth: bounded(
                  width + (event.key === 'ArrowRight' ? 10 : -10) * sign,
                ),
              })
            } else if (event.key === 'Home') {
              event.preventDefault()
              update({ conversationWidth: 640 })
            } else if (event.key === 'End') {
              event.preventDefault()
              update({ conversationWidth: maximum })
            } else if (event.key === 'Escape') {
              event.preventDefault()
              finish(false)
            }
          }}
          onDoubleClick={() => update({ conversationWidth: null })}
          onPointerDown={(event) => {
            if (event.button !== 0) return
            event.preventDefault()
            event.currentTarget.setPointerCapture(event.pointerId)
            drag.current = {
              x: event.clientX,
              latest: event.clientX,
              base: width,
              side: side === 'right' ? 1 : -1,
            }
          }}
          onPointerMove={(event) => {
            const current = drag.current
            if (
              !current ||
              !event.currentTarget.hasPointerCapture(event.pointerId)
            )
              return
            current.latest = event.clientX
            setPreview(
              bounded(
                current.base + (current.latest - current.x) * current.side * 2,
              ),
            )
          }}
          onPointerUp={(event) => {
            if (drag.current) drag.current.latest = event.clientX
            finish(true)
          }}
          onPointerCancel={() => finish(false)}
          onLostPointerCapture={() => finish(false)}
        />
      ))}
    </div>
  )
}
