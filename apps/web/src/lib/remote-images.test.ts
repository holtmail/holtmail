import { describe, expect, it } from 'bun:test'

import { bodyMode, parseRemoteImages } from './remote-images.ts'

describe('parseRemoteImages', () => {
  it('falls back to asking', () => {
    expect(parseRemoteImages(null)).toBe('ask')
    expect(parseRemoteImages('always')).toBe('ask')
    expect(parseRemoteImages('proxy')).toBe('proxy')
    expect(parseRemoteImages('direct')).toBe('direct')
  })
})

describe('bodyMode', () => {
  it('blocks until shown, then goes through the server', () => {
    expect(bodyMode('ask', false)).toBe('block')
    expect(bodyMode('ask', true)).toBe('proxy')
  })

  it('follows a standing choice', () => {
    expect(bodyMode('proxy', false)).toBe('proxy')
    expect(bodyMode('direct', false)).toBe('direct')
  })
})
