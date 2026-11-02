import { transition, distance } from '@holtmail/tokens'
import { m } from 'motion/react'
import type { ReactNode } from 'react'

export function EmptyState({
  icon,
  title,
  children,
}: {
  icon: ReactNode
  title: string
  children?: ReactNode
}) {
  return (
    <m.div
      className="flex h-full flex-col items-center justify-center gap-3 px-8 py-16 text-center"
      initial={{ opacity: 0, y: distance.xs }}
      animate={{ opacity: 1, y: 0, transition: transition.expressive.slow01 }}
    >
      <div className="text-fg-subtle [&_svg]:size-8 [&_svg]:stroke-1">{icon}</div>
      <p className="text-base text-fg">{title}</p>
      {children && <div className="max-w-xs text-sm text-fg-muted">{children}</div>}
    </m.div>
  )
}
