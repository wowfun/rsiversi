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
    pointerId: number
    x: number
    base: number
    latest: number
    side: number
    frame?: number
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
    if (!current) return
    if (current.frame !== undefined) cancelAnimationFrame(current.frame)
    drag.current = undefined
    setPreview(undefined)
    if (commit && current.latest !== current.x)
      update({
        conversationWidth: bounded(
          current.base + (current.latest - current.x) * current.side * 2,
        ),
      })
  }
  // At small widths the transcript already occupies its column's usable space.
  const handles = showHandles && column - width > 96
  useLayoutEffect(() => () => {
    if (!drag.current) return
    if (drag.current.frame !== undefined) cancelAnimationFrame(drag.current.frame)
    drag.current = undefined
    setPreview(undefined)
  },[container,handles])
  if (!handles) return null
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
            if (event.button !== 0 || drag.current) return
            event.preventDefault()
            event.currentTarget.setPointerCapture(event.pointerId)
            drag.current = {
              pointerId: event.pointerId,
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
            event.currentTarget.style.setProperty('--handle-y', `${event.clientY-event.currentTarget.getBoundingClientRect().top}px`)
            current.frame ??= requestAnimationFrame(() => {
              current.frame = undefined
              setPreview(bounded(current.base + (current.latest - current.x) * current.side * 2))
            })
          }}
          onPointerUp={(event) => {
            if (drag.current?.pointerId !== event.pointerId) return
            drag.current.latest = event.clientX
            finish(true)
          }}
          onPointerCancel={event => { if (drag.current?.pointerId === event.pointerId) finish(false) }}
          onLostPointerCapture={event => { if (drag.current?.pointerId === event.pointerId) finish(false) }}
          onWheel={(event) => {
            if (event.ctrlKey || event.deltaY === 0) return
            const port = container?.querySelector<HTMLElement>('.transcript')
            if (!port) return
            const unit = event.deltaMode === WheelEvent.DOM_DELTA_PAGE ? port.clientHeight
              : event.deltaMode === WheelEvent.DOM_DELTA_LINE ? parseFloat(getComputedStyle(port).lineHeight) || 16 : 1
            port.scrollBy({top:event.deltaY * unit})
          }}
        />
      ))}
    </div>
  )
}
