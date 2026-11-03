import { distance, duration, transition } from '@holtmail/tokens'
import { m, type Variants } from 'motion/react'
import type { ReactNode } from 'react'

import { strings } from '../strings.ts'

const column: Variants = {
  shown: { transition: { staggerChildren: duration.fast01 / 1000 } },
}

const item: Variants = {
  hidden: { opacity: 0, y: distance.sm },
  shown: { opacity: 1, y: 0, transition: transition.expressive.slow01 },
}

export function Wordmark() {
  return (
    <div className="grid w-fit gap-2">
      <div className="flex items-center gap-3">
        <span aria-hidden className="size-5 bg-accent" />
        <span className="text-lg font-semibold">{strings.appName}</span>
      </div>
      <m.span
        aria-hidden
        className="h-px origin-left bg-border-strong"
        initial={{ scaleX: 0 }}
        animate={{ scaleX: 1, transition: transition.expressive.slow01 }}
      />
      <span className="font-mono text-xs text-fg-subtle">{strings.auth.tagline}</span>
    </div>
  )
}

export function AuthScreen({
  title,
  lead,
  children,
}: {
  title: string
  lead: ReactNode
  children?: ReactNode
}) {
  return (
    <main className="flex min-h-full items-center justify-center bg-bg px-4 py-16 text-fg">
      <m.div className="w-full max-w-sm" initial="hidden" animate="shown" variants={column}>
        <m.div variants={item}>
          <Wordmark />
        </m.div>
        <m.h1 variants={item} className="mt-16 text-2xl font-normal text-fg">
          {title}
        </m.h1>
        <m.div variants={item} className="mt-2 text-sm text-fg-muted">
          {lead}
        </m.div>
        {children && (
          <m.div variants={item} className="mt-8">
            {children}
          </m.div>
        )}
      </m.div>
    </main>
  )
}
