import { describe, expect, test } from 'bun:test'

import { ApiError } from '@holtmail/api-client'

import {
  authErrorMessage,
  initials,
  parseLoginSearch,
  parseSetupSearch,
  passwordShortBy,
  retryMinutes,
  safeNext,
  sentence,
} from './auth.ts'

describe('auth helpers', () => {
  test('next stays on this site and never loops back to sign-in', () => {
    expect(safeNext('/a/1/f/2?m=3')).toBe('/a/1/f/2?m=3')
    expect(safeNext('https://example.com/')).toBe('/')
    expect(safeNext('//example.com/')).toBe('/')
    expect(safeNext('/\\example.com')).toBe('/')
    expect(safeNext('/login?next=/a')).toBe('/')
    expect(safeNext('/setup')).toBe('/')
    expect(safeNext(undefined)).toBe('/')
  })

  test('search params', () => {
    expect(parseLoginSearch({ next: '/a/1' })).toEqual({ next: '/a/1' })
    expect(parseLoginSearch({ next: '//evil.example.com' })).toEqual({})
    expect(parseLoginSearch({ next: 5 })).toEqual({})
    expect(parseSetupSearch({ token: ' abc ' })).toEqual({ token: 'abc' })
    expect(parseSetupSearch({})).toEqual({})
  })

  test('password length counts characters, not bytes', () => {
    expect(passwordShortBy('')).toBe(10)
    expect(passwordShortBy('ööööö')).toBe(5)
    expect(passwordShortBy('analytical engine')).toBe(0)
  })

  test('retry minutes round up and never show zero', () => {
    expect(retryMinutes(900)).toBe(15)
    expect(retryMinutes(61)).toBe(2)
    expect(retryMinutes(0)).toBe(1)
    expect(retryMinutes(undefined)).toBe(1)
  })

  test('sentence and initials', () => {
    expect(sentence('wrong email or password')).toBe('Wrong email or password.')
    expect(sentence('Done!')).toBe('Done!')
    expect(initials('ada Lovelace', 'ada@example.com')).toBe('A')
    expect(initials(' ', 'bob@example.org')).toBe('B')
  })

  test('error messages', () => {
    expect(authErrorMessage(new ApiError(429, 'too many', 600))).toBe(
      'Too many attempts, try again in 10 minutes.',
    )
    expect(authErrorMessage(new ApiError(429, 'too many', 30))).toBe(
      'Too many attempts, try again in 1 minute.',
    )
    expect(authErrorMessage(new ApiError(401, 'wrong email or password'))).toBe(
      'Wrong email or password.',
    )
    expect(authErrorMessage(new ApiError(500, 'sign-in failed'))).toBe(
      'Something went wrong. Try again.',
    )
    expect(authErrorMessage(new TypeError('offline'))).toBe('Something went wrong. Try again.')
  })
})
