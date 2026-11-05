import type { Task } from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import { cn } from '@holtmail/ui'
import { m } from 'motion/react'

import { taskShare } from '../lib/tasks.ts'

export function TaskProgress({ task, className }: { task: Task; className?: string }) {
  const share = taskShare(task)
  const tone =
    task.state === 'failed'
      ? 'bg-danger'
      : task.state === 'cancelled'
        ? 'bg-fg-subtle'
        : task.state === 'interrupted'
          ? 'bg-warning'
          : task.state === 'done'
            ? 'bg-success'
            : 'bg-accent'
  return (
    <div
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={task.total ?? undefined}
      aria-valuenow={task.total === null ? undefined : task.done}
      aria-valuetext={task.summary}
      className={cn('relative h-1 w-full overflow-hidden bg-bg-selected', className)}
    >
      {share === undefined && task.state === 'running' ? (
        <m.span
          className={cn('absolute inset-y-0 w-1/3', tone)}
          initial={{ left: '-33%' }}
          animate={{ left: '100%' }}
          transition={{ duration: 1.4, repeat: Infinity, ease: 'easeInOut' }}
        />
      ) : (
        <m.span
          className={cn('absolute inset-0 origin-left', tone)}
          initial={false}
          animate={{
            scaleX: share ?? 1,
            transition: transition.productive.moderate01,
          }}
        />
      )}
    </div>
  )
}
