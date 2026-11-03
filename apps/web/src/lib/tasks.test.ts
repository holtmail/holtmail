import { describe, expect, test } from 'bun:test'

import type { Task } from '@holtmail/api-client'

import { formatDuration, remoteImagesTask, runningTasks, taskShare } from './tasks.ts'

const task = (change: Partial<Task>): Task => ({
  id: 't1',
  kind: 'remote_images',
  state: 'running',
  title: 'News',
  archive_id: 'a1',
  account_id: null,
  started_at: 0,
  finished_at: null,
  done: 0,
  total: null,
  summary: '',
  cancellable: true,
  log: [],
  ...change,
})

describe('taskShare', () => {
  test('is unknown without a total and clamped with one', () => {
    expect(taskShare(task({}))).toBeUndefined()
    expect(taskShare(task({ done: 5, total: 20 }))).toBe(0.25)
    expect(taskShare(task({ done: 30, total: 20 }))).toBe(1)
    expect(taskShare(task({ total: 0 }))).toBe(0)
    expect(taskShare(task({ total: 0, state: 'done' }))).toBe(1)
  })
})

test('formatDuration', () => {
  expect(formatDuration(4_400)).toBe('4 s')
  expect(formatDuration(125_000)).toBe('2 min 5 s')
  expect(formatDuration(3_600_000 * 2 + 60_000 * 3)).toBe('2 h 3 min')
})

test('finds the running remote image task of an archive', () => {
  const tasks = [
    task({ id: 't1', state: 'done' }),
    task({ id: 't2', kind: 'sync' }),
    task({ id: 't3' }),
    task({ id: 't4', archive_id: 'a2' }),
  ]
  expect(runningTasks(tasks).map((t) => t.id)).toEqual(['t2', 't3', 't4'])
  expect(remoteImagesTask(tasks, 'a1')?.id).toBe('t3')
  expect(remoteImagesTask(tasks, 'a3')).toBeUndefined()
})
