import { expect, test } from 'bun:test'

import { manages } from './roles.ts'

test('owners and admins manage the instance, members do not', () => {
  expect(manages('owner')).toBe(true)
  expect(manages('admin')).toBe(true)
  expect(manages('member')).toBe(false)
  expect(manages(undefined)).toBe(false)
})
