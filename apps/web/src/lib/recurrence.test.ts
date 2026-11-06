import { describe, expect, test } from 'bun:test'

import type { Recurrence } from '@holtmail/api-client'

import { describeRecurrence } from './recurrence.ts'

function rule(parts: Partial<Recurrence>): Recurrence {
  return {
    rule: 'FREQ=WEEKLY',
    frequency: 'WEEKLY',
    interval: 1,
    count: null,
    until: null,
    by_day: [],
    by_month_day: [],
    by_month: [],
    by_set_pos: [],
    by_year_day: [],
    by_week_no: [],
    by_hour: [],
    by_minute: [],
    week_start: 'MO',
    rdates: [],
    exdates: [],
    changed: [],
    first_start: null,
    last_end: null,
    ...parts,
  }
}

const day = (weekday: string, ordinal: number | null = null) => ({ weekday, ordinal })

describe('describeRecurrence', () => {
  test('weekly on days with an interval and a count', () => {
    expect(describeRecurrence(rule({ by_day: [day('MO')] }), 'en-US')).toBe('Every week on Monday')
    expect(
      describeRecurrence(rule({ interval: 2, by_day: [day('TH'), day('TU')], count: 5 }), 'en-US'),
    ).toBe('Every 2 weeks on Tuesday and Thursday, 5 times')
  })

  test('monthly by ordinal weekday, set position and month day', () => {
    expect(
      describeRecurrence(rule({ frequency: 'MONTHLY', by_day: [day('FR', -1)] }), 'en-US'),
    ).toBe('Every month on the last Friday')
    expect(
      describeRecurrence(
        rule({
          frequency: 'MONTHLY',
          by_day: ['MO', 'TU', 'WE', 'TH', 'FR'].map((d) => day(d)),
          by_set_pos: [-1],
        }),
        'en-US',
      ),
    ).toBe('Every month on the last weekday')
    expect(describeRecurrence(rule({ frequency: 'MONTHLY', by_month_day: [15] }), 'en-US')).toBe(
      'Every month on day 15',
    )
    expect(describeRecurrence(rule({ frequency: 'MONTHLY', by_month_day: [-1] }), 'en-US')).toBe(
      'Every month on the last day',
    )
  })

  test('yearly forms and an end date', () => {
    expect(
      describeRecurrence(
        rule({ frequency: 'YEARLY', by_month: [11], by_day: [day('TH', 4)] }),
        'en-US',
      ),
    ).toBe('Every year on the fourth Thursday of November')
    expect(
      describeRecurrence(
        rule({ frequency: 'YEARLY', by_month: [2], by_month_day: [29], until: '20320301T000000Z' }),
        'en-US',
      ),
    ).toBe('Every year on 29 February, until March 1, 2032')
    expect(describeRecurrence(rule({ frequency: 'DAILY', interval: 3 }), 'en-US')).toBe(
      'Every 3 days',
    )
  })

  test('says so when the rule cannot be read', () => {
    expect(describeRecurrence(rule({ frequency: null, rule: 'FREQ=FORTNIGHTLY' }))).toBe(
      'Repeats by a rule Holtmail cannot read',
    )
    expect(describeRecurrence(rule({ frequency: null, rule: null }))).toBe('On the dates listed')
  })
})
