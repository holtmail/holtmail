import { distance, transition } from '@holtmail/tokens'
import { domAnimation, LazyMotion, MotionConfig } from 'motion/react'
import type { ReactNode } from 'react'

export function MotionProvider({ children }: { children: ReactNode }) {
  return (
    <LazyMotion features={domAnimation} strict>
      <MotionConfig reducedMotion="user">{children}</MotionConfig>
    </LazyMotion>
  )
}

export const fade = {
  initial: { opacity: 0 },
  animate: { opacity: 1, transition: transition.productive.moderate01 },
  exit: { opacity: 0, transition: transition.productiveExit.fast02 },
} as const

export const rise = {
  initial: { opacity: 0, y: distance.sm },
  animate: { opacity: 1, y: 0, transition: transition.productive.moderate01 },
  exit: { opacity: 0, y: distance.sm, transition: transition.productiveExit.fast02 },
} as const

export const drop = {
  initial: { opacity: 0, y: -distance.xs },
  animate: { opacity: 1, y: 0, transition: transition.productive.fast02 },
  exit: { opacity: 0, transition: transition.productiveExit.fast01 },
} as const

export const hint = {
  initial: { opacity: 0 },
  animate: { opacity: 1, transition: transition.productive.fast02 },
  exit: { opacity: 0, transition: transition.productiveExit.fast01 },
} as const
