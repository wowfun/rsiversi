import type { IconProps } from './props.ts'
interface WeightedIconProps extends IconProps { strokeWidth: number }
const ICON_REGULAR_STROKE = 1


const IconPanelLeftOutlineArtwork = ({ size = 16, className, strokeWidth }: WeightedIconProps) => (
  <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true" strokeWidth={strokeWidth}>
    <path d="M13.5 1.5H2.5C1.94772 1.5 1.5 1.94772 1.5 2.5V13.5C1.5 14.0523 1.94772 14.5 2.5 14.5H13.5C14.0523 14.5 14.5 14.0523 14.5 13.5V2.5C14.5 1.94772 14.0523 1.5 13.5 1.5Z" stroke="currentColor" />
    <path d="M5.5 1.5V14.5" stroke="currentColor" />
  </svg>
)

export const IconPanelLeftOutlineRegular = (props: IconProps) => (
  <IconPanelLeftOutlineArtwork {...props} strokeWidth={ICON_REGULAR_STROKE} />
)

const IconPlusOutlineArtwork = ({ size = 16, className, strokeWidth }: WeightedIconProps) => (
  <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true" strokeWidth={strokeWidth}>
    <path d="M8 2V14" stroke="currentColor" />
    <path d="M2 8H14" stroke="currentColor" />
  </svg>
)

export const IconPlusOutlineRegular = (props: IconProps) => (
  <IconPlusOutlineArtwork {...props} strokeWidth={ICON_REGULAR_STROKE} />
)

const IconCloseOutlineArtwork = ({ size = 16, className, strokeWidth }: WeightedIconProps) => (
  <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true" strokeWidth={strokeWidth}>
    <path d="M2.5 2.5L13.5 13.5" stroke="currentColor" />
    <path d="M13.5 2.5L2.5 13.5" stroke="currentColor" />
  </svg>
)

export const IconCloseOutlineRegular = (props: IconProps) => (
  <IconCloseOutlineArtwork {...props} strokeWidth={ICON_REGULAR_STROKE} />
)

const IconCloseFillArtwork = ({ size = 14, className, strokeWidth }: WeightedIconProps) => (
  <svg width={size} height={size} className={className} viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg" aria-hidden="true" strokeWidth={strokeWidth}>
    <path d="M3.5 3.5L12.5 12.5" stroke="currentColor" />
    <path d="M12.5 3.5L3.5 12.5" stroke="currentColor" />
  </svg>
)

export const IconCloseFillRegular = (props: IconProps) => (
  <IconCloseFillArtwork {...props} strokeWidth={ICON_REGULAR_STROKE} />
)
