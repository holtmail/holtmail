import { describe, expect, test } from 'bun:test'

import { type CopyEnv, copyText, manualCopyHint } from './copy.ts'

function fakeDocument(execResult: boolean | 'throw', body = true) {
  const log: string[] = []
  const area = {
    value: '',
    readOnly: false,
    tabIndex: 0,
    style: {} as Record<string, string>,
    setAttribute: () => {},
    select: () => log.push('select'),
    setSelectionRange: () => {},
  }
  const active = { focus: () => log.push('refocus') }
  const doc = {
    activeElement: active,
    body: body
      ? {
          appendChild: () => log.push('append'),
          removeChild: () => log.push('remove'),
        }
      : null,
    createElement: () => area,
    execCommand: (command: string) => {
      log.push(`exec:${command}:${area.value}`)
      if (execResult === 'throw') throw new Error('blocked')
      return execResult
    },
  }
  return { doc: doc as unknown as Document, log, area }
}

describe('copyText', () => {
  test('uses the Clipboard API first', async () => {
    const written: string[] = []
    const { doc, log } = fakeDocument(true)
    const env: CopyEnv = {
      clipboard: { writeText: async (text) => void written.push(text) },
      document: doc,
    }
    expect(await copyText('482913', env)).toBe('clipboard')
    expect(written).toEqual(['482913'])
    expect(log).toEqual([])
  })

  test('falls back to execCommand when the Clipboard API rejects', async () => {
    const { doc, log, area } = fakeDocument(true)
    const env: CopyEnv = {
      clipboard: { writeText: () => Promise.reject(new Error('denied')) },
      document: doc,
    }
    expect(await copyText('482913', env)).toBe('exec')
    expect(log).toEqual(['append', 'select', 'exec:copy:482913', 'remove', 'refocus'])
    expect(area.readOnly).toBe(true)
  })

  test('falls back to execCommand when the Clipboard API throws synchronously', async () => {
    const { doc } = fakeDocument(true)
    const env: CopyEnv = {
      clipboard: {
        writeText: () => {
          throw new Error('denied')
        },
      },
      document: doc,
    }
    expect(await copyText('x', env)).toBe('exec')
  })

  test('uses execCommand when there is no Clipboard API, as on plain HTTP', async () => {
    const { doc } = fakeDocument(true)
    expect(await copyText('x', { clipboard: null, document: doc })).toBe('exec')
  })

  test('is manual when execCommand returns false', async () => {
    const { doc, log } = fakeDocument(false)
    expect(await copyText('x', { document: doc })).toBe('manual')
    expect(log).toContain('remove')
  })

  test('is manual when execCommand throws, and still removes the textarea', async () => {
    const { doc, log } = fakeDocument('throw')
    expect(await copyText('x', { document: doc })).toBe('manual')
    expect(log.slice(-2)).toEqual(['remove', 'refocus'])
  })

  test('is manual without a body', async () => {
    const { doc, log } = fakeDocument(true, false)
    expect(await copyText('x', { document: doc })).toBe('manual')
    expect(log).toEqual([])
  })

  test('is manual with nothing available', async () => {
    expect(await copyText('x', {})).toBe('manual')
  })
})

describe('manualCopyHint', () => {
  test('names the platform shortcut', () => {
    expect(manualCopyHint('MacIntel')).toBe('Press ⌘C')
    expect(manualCopyHint('iPhone')).toBe('Press ⌘C')
    expect(manualCopyHint('Win32')).toBe('Press Ctrl+C')
    expect(manualCopyHint('Linux x86_64')).toBe('Press Ctrl+C')
  })
})
