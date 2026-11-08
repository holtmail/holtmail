import { describe, expect, test } from 'bun:test'

import {
  applyOverlay,
  flagsOnly,
  mergeOverlay,
  OVERLAY_MS,
  OverlayStore,
  overlayHeld,
} from './overlay.ts'

describe('overlay', () => {
  test('later flag changes win over earlier ones', () => {
    const read = mergeOverlay(undefined, { kind: 'flags', add: ['\\Seen'] }, [], 1)
    const unread = mergeOverlay(read, { kind: 'flags', remove: ['\\seen'] }, [], 2)
    expect(unread.add).toEqual([])
    expect(unread.remove).toEqual(['\\seen'])
    expect(applyOverlay(['\\Seen', '\\Flagged'], unread)).toEqual(['\\Flagged'])
  })

  test('a row is held until the server shows the change', () => {
    const entry = mergeOverlay(undefined, { kind: 'flags', add: ['\\Seen'] }, [], 1)
    expect(overlayHeld([], entry)).toBe(true)
    expect(overlayHeld(['\\SEEN'], entry)).toBe(false)
    expect(overlayHeld([], undefined)).toBe(false)
  })

  test('a move holds and marks rows only in the folder it leaves', () => {
    const entry = mergeOverlay(undefined, { kind: 'move', folder: 'arch' }, ['in'], 1)
    expect(overlayHeld([], entry, 'in')).toBe(true)
    expect(overlayHeld([], entry, 'arch')).toBe(false)
    expect(overlayHeld([], entry)).toBe(false)
  })

  test('moving back into a folder it left stops holding rows there', () => {
    const out = mergeOverlay(undefined, { kind: 'move', folder: 'arch' }, ['in'], 1)
    const back = mergeOverlay(out, { kind: 'move', folder: 'in' }, ['arch'], 2)
    expect(back.leaves).toEqual(['arch'])
    expect(overlayHeld([], back, 'in')).toBe(false)
    expect(flagsOnly(back)).toBe(false)
    expect(flagsOnly(mergeOverlay(undefined, { kind: 'flags', add: ['\\Seen'] }, [], 1))).toBe(true)
    expect(flagsOnly(undefined)).toBe(false)
  })

  test('the store notifies and forgets entries after the settle time', () => {
    const store = new OverlayStore()
    let calls = 0
    const stop = store.subscribe(() => {
      calls += 1
    })
    store.record('m1', { kind: 'flags', add: ['\\Flagged'] }, [], 1000)
    expect(calls).toBe(1)
    expect(store.snapshot()).toBe(1)
    expect(store.get('m1', 1000 + OVERLAY_MS)?.add).toEqual(['\\Flagged'])
    expect(store.get('m1', 1001 + OVERLAY_MS)).toBeUndefined()
    store.forget('m1')
    expect(calls).toBe(1)
    stop()
  })
})
