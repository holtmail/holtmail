import { archivesQuery, cancelTask, type Task, tasksQuery } from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import { Badge, type BadgeProps, Button, cn, Skeleton } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ChevronDownIcon,
  FileArchiveIcon,
  ImageIcon,
  type LucideIcon,
  RefreshCwIcon,
  SquareIcon,
} from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { useEffect, useRef, useState } from 'react'

import { useNow } from '../hooks/use-now.ts'
import { formatAgo } from '../lib/accounts.ts'
import { formatDuration } from '../lib/tasks.ts'
import { strings } from '../strings.ts'
import { TaskProgress } from '../tasks/task-progress.tsx'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings

const icons: Record<string, LucideIcon | undefined> = {
  sync: RefreshCwIcon,
  import: FileArchiveIcon,
  remote_images: ImageIcon,
}

const stateBadge: Record<string, BadgeProps['variant'] | undefined> = {
  running: 'accent',
  done: 'success',
  failed: 'danger',
  cancelled: 'outline',
}

const kindLabels: Record<string, string | undefined> = t.taskKinds
const stateLabels: Record<string, string | undefined> = t.taskStates
const kindLabel = (kind: string) => kindLabels[kind] ?? kind
const stateLabel = (state: string) => stateLabels[state] ?? state

export function TasksPage() {
  const tasks = useQuery(tasksQuery)
  const archives = useQuery(archivesQuery)
  const names = new Map(archives.data?.map((archive) => [archive.id, archive.name]))
  return (
    <SettingsPage title={t.tasks} lead={t.tasksLead}>
      {tasks.isPending && <Skeleton className="h-32" />}
      {tasks.data?.length === 0 && <p className="text-sm text-fg-subtle">{t.tasksEmpty}</p>}
      <ul className="grid divide-y divide-border border-y border-border">
        <AnimatePresence initial={false}>
          {tasks.data?.map((task) => (
            <m.li
              key={task.id}
              layout
              initial={{ opacity: 0 }}
              animate={{ opacity: 1, transition: transition.productive.moderate01 }}
              exit={{ opacity: 0, transition: transition.productiveExit.fast02 }}
            >
              <TaskRow
                task={task}
                archive={task.archive_id ? names.get(task.archive_id) : undefined}
              />
            </m.li>
          ))}
        </AnimatePresence>
      </ul>
      <p className="text-xs text-fg-subtle">{t.tasksNote}</p>
    </SettingsPage>
  )
}

function TaskRow({ task, archive }: { task: Task; archive: string | undefined }) {
  const now = useNow(1_000)
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(task.state === 'failed')
  const Icon = icons[task.kind] ?? SquareIcon
  const running = task.state === 'running'
  const cancel = useMutation({
    mutationFn: () => cancelTask(task.id),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ['tasks'] }),
  })
  const logId = `task-log-${task.id}`
  const timing = running
    ? t.taskRunningFor(formatDuration(now - task.started_at))
    : task.finished_at
      ? t.taskTook(formatDuration(task.finished_at - task.started_at))
      : ''

  return (
    <div className="grid gap-3 py-4">
      <div className="flex items-start gap-4">
        <span
          aria-hidden
          className="flex size-8 shrink-0 items-center justify-center bg-bg-subtle text-fg-muted [&_svg]:size-4"
        >
          <Icon className={cn(running && task.kind === 'sync' && 'animate-spin')} />
        </span>
        <div className="grid min-w-0 flex-1 gap-1">
          <div className="flex min-w-0 flex-wrap items-center gap-2">
            <span className="truncate text-sm text-fg">
              {kindLabel(task.kind)} · {task.title}
            </span>
            <Badge variant={stateBadge[task.state] ?? 'outline'}>{stateLabel(task.state)}</Badge>
          </div>
          <span className="truncate text-xs text-fg-muted">{task.summary}</span>
          <span className="font-mono text-xs text-fg-subtle tabular-nums">
            {[archive, t.taskStarted(formatAgo(task.started_at, now)), timing]
              .filter(Boolean)
              .join(' · ')}
          </span>
        </div>
        {running && task.cancellable && (
          <Button
            variant="ghost"
            size="sm"
            disabled={cancel.isPending}
            onClick={() => cancel.mutate()}
          >
            <SquareIcon />
            {t.cancel}
          </Button>
        )}
      </div>
      <TaskProgress task={task} />
      {task.log.length > 0 && (
        <div className="grid gap-2">
          <button
            type="button"
            aria-expanded={open}
            aria-controls={logId}
            onClick={() => setOpen((value) => !value)}
            className="flex w-fit items-center gap-1 text-xs text-fg-muted outline-none hover:text-fg focus-visible:outline-2 focus-visible:outline-focus"
          >
            <ChevronDownIcon
              aria-hidden
              className={cn(
                'size-3.5 transition-transform duration-fast-02 ease-productive',
                open && 'rotate-180',
              )}
            />
            {open ? t.hideLog : t.showLog} ({task.log.length})
          </button>
          <AnimatePresence initial={false}>
            {open && (
              <m.div
                key="log"
                id={logId}
                className="overflow-hidden"
                initial={{ height: 0, opacity: 0 }}
                animate={{
                  height: 'auto',
                  opacity: 1,
                  transition: transition.productive.moderate01,
                }}
                exit={{ height: 0, opacity: 0, transition: transition.productiveExit.fast02 }}
              >
                <TaskLog task={task} />
              </m.div>
            )}
          </AnimatePresence>
        </div>
      )}
    </div>
  )
}

const levelClass: Record<string, string> = {
  info: 'text-fg-muted',
  warning: 'text-warning',
  error: 'text-danger',
}

function TaskLog({ task }: { task: Task }) {
  const box = useRef<HTMLOListElement>(null)
  const atEnd = useRef(true)
  useEffect(() => {
    const element = box.current
    if (element && atEnd.current) element.scrollTop = element.scrollHeight
  }, [task.log.length])
  return (
    <ol
      ref={box}
      aria-live="off"
      onScroll={(event) => {
        const element = event.currentTarget
        atEnd.current = element.scrollHeight - element.scrollTop - element.clientHeight < 8
      }}
      className="max-h-64 overflow-y-auto border border-border bg-bg-subtle px-3 py-2 font-mono text-xs leading-5"
    >
      {task.log.map((line, index) => (
        <li key={index} className="grid grid-cols-[auto_1fr] gap-3">
          <time dateTime={new Date(line.at).toISOString()} className="text-fg-subtle tabular-nums">
            {new Date(line.at).toLocaleTimeString()}
          </time>
          <span className={cn('break-words', levelClass[line.level] ?? 'text-fg-muted')}>
            {line.text}
          </span>
        </li>
      ))}
    </ol>
  )
}
