import {useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode} from 'react'
import {IconChevronLeftOutlineRegular, IconChevronRightOutlineRegular} from '../dockkit-primitives/icons/index.tsx'
import css from './AttachmentRail.module.css'

/** Browser-owned scrolling for the composer's ordered attachment cards. */
export function AttachmentRail({count, children}: {count: number; children: ReactNode}) {
  const rail = useRef<HTMLDivElement>(null)
  const previousCount = useRef(count)
  const [edges, setEdges] = useState({left: false, right: false})

  const updateEdges = useCallback((element: HTMLDivElement) => {
    const left = element.scrollLeft > 1
    const right = element.scrollWidth - element.clientWidth - element.scrollLeft > 1
    setEdges(previous => previous.left === left && previous.right === right ? previous : {left, right})
  }, [])

  useLayoutEffect(() => {
    const element = rail.current!
    if (count > previousCount.current) element.scrollLeft = element.scrollWidth - element.clientWidth
    previousCount.current = count
    updateEdges(element)
  }, [count, updateEdges])

  useEffect(() => {
    const element = rail.current!
    const observer = new ResizeObserver(() => updateEdges(element))
    observer.observe(element)
    const pan = (event: WheelEvent) => {
      if (event.deltaY === 0) return
      const unit = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? 16 :
        event.deltaMode === WheelEvent.DOM_DELTA_PAGE ? element.clientWidth : 1
      event.preventDefault()
      const left = event.deltaX ? event.deltaX * unit :
        Math.sign(event.deltaY) * Math.min(Math.abs(event.deltaY) * unit, 60)
      element.scrollBy({left, behavior: 'auto'})
    }
    element.addEventListener('wheel', pan, {passive: false})
    return () => {
      observer.disconnect()
      element.removeEventListener('wheel', pan)
    }
  }, [updateEdges])

  function page(direction: number) {
    const element = rail.current!
    const step = Math.max(element.clientWidth - 64, 200)
    const behavior = matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth'
    element.scrollBy({left: direction * step, behavior})
  }

  return <div className={css.root}>
    {edges.left && <button type="button" className={css.arrow + ' ' + css.arrowLeft}
      aria-label="Scroll attachments left" onClick={() => page(-1)}><IconChevronLeftOutlineRegular/></button>}
    <div ref={rail} className={css.rail} role="group" aria-label="Pending attachments" onScroll={event => updateEdges(event.currentTarget)}>
      {children}
    </div>
    {edges.right && <button type="button" className={css.arrow + ' ' + css.arrowRight}
      aria-label="Scroll attachments right" onClick={() => page(1)}><IconChevronRightOutlineRegular/></button>}
  </div>
}
