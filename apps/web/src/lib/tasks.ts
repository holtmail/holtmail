import type { Task } from '@holtmail/api-client'

export function taskShare(task: Task): number | undefined {
  if (task.total === null || task.total === undefined) return undefined
  if (task.total === 0) return task.state === 'running' ? 0 : 1
  return Math.min(1, task.done / task.total)
}

export function formatDuration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000))
  if (seconds < 60) return `${seconds} s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return seconds % 60 ? `${minutes} min ${seconds % 60} s` : `${minutes} min`
  const hours = Math.floor(minutes / 60)
  return minutes % 60 ? `${hours} h ${minutes % 60} min` : `${hours} h`
}

export function runningTasks(tasks: Task[] | undefined): Task[] {
  return (tasks ?? []).filter((task) => task.state === 'running')
}

export function remoteImagesTask(tasks: Task[] | undefined, archiveId: string): Task | undefined {
  return runningTasks(tasks).find(
    (task) => task.kind === 'remote_images' && task.archive_id === archiveId,
  )
}
