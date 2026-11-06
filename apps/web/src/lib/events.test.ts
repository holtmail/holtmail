import { describe, expect, test } from 'bun:test'

import { changeKind, fieldLabel, formatWhen, participantLabel, readEvent } from './events.ts'

const berlin = { value: '2025-11-10T14:00:00', time_zone: 'Europe/Berlin', utc: 1_762_779_600_000 }
const berlinEnd = {
  value: '2025-11-10T15:00:00',
  time_zone: 'Europe/Berlin',
  utc: 1_762_783_200_000,
}

describe('formatWhen', () => {
  test('shows the viewer zone and the original zone when they differ', () => {
    const when = formatWhen(berlin, berlinEnd, false, 'America/New_York', 'en-US')
    expect(when?.local).toContain('8:00')
    expect(when?.local).toContain('9:00')
    expect(when?.original).toContain('2:00')
    expect(when?.original).toEndWith('(Europe/Berlin)')
  })

  test('leaves out the original zone when it reads the same', () => {
    expect(formatWhen(berlin, undefined, false, 'Europe/Berlin', 'en-US')?.original).toBeUndefined()
    expect(formatWhen(berlin, undefined, false, 'Europe/Paris', 'en-US')?.original).toBeUndefined()
  })

  test('keeps all-day dates and floating times as written', () => {
    const day = formatWhen(
      { value: '2025-11-10', time_zone: null, utc: 1_762_732_800_000 },
      { value: '2025-11-11', time_zone: null, utc: 1_762_819_200_000 },
      true,
      'Pacific/Auckland',
      'en-US',
    )
    expect(day).toEqual({ local: 'Monday, November 10, 2025' })
    const floating = formatWhen(
      { value: '2025-11-10T09:30:00', time_zone: null, utc: 1_762_767_000_000 },
      undefined,
      false,
      'Asia/Tokyo',
      'en-US',
    )
    expect(floating?.local).toContain('9:30')
    expect(floating?.original).toBeUndefined()
  })

  test('shows an unknown zone as written', () => {
    const when = formatWhen(
      { value: '2025-11-10T09:00:00', time_zone: 'Harbour Standard Time', utc: 1_762_765_200_000 },
      undefined,
      false,
      'UTC',
      'en-US',
    )
    expect(when?.original).toBe('2025-11-10T09:00:00 Harbour Standard Time')
    expect(formatWhen(undefined, undefined, false, 'UTC')).toBeUndefined()
  })
})

describe('readEvent', () => {
  test('reads the main component of a calendar view', () => {
    const event = readEvent({
      type: 'calendar',
      components: [
        {
          summary: 'Harbour planning',
          start: berlin,
          all_day: false,
          status: 'CANCELLED',
          organizer: { email: 'ada@example.com', name: 'Ada Quill' },
          attendees: [{ email: 'bob@example.org', partstat: 'ACCEPTED' }, 'junk'],
        },
      ],
    })
    expect(event?.summary).toBe('Harbour planning')
    expect(event?.status).toBe('cancelled')
    expect(event?.start?.time_zone).toBe('Europe/Berlin')
    expect(event?.attendees).toEqual([
      { email: 'bob@example.org', name: undefined, status: 'accepted' },
    ])
    expect(participantLabel(event?.organizer ?? {})).toBe('Ada Quill <ada@example.com>')
  })

  test('gives nothing for contacts and unreadable versions', () => {
    expect(readEvent({ type: 'contact' })).toBeUndefined()
    expect(readEvent({ type: 'calendar', components: [] })).toBeUndefined()
    expect(readEvent(null)).toBeUndefined()
  })
})

describe('changes', () => {
  test('names fields and instances', () => {
    expect(fieldLabel('start')).toBe('Start')
    expect(fieldLabel('full_name')).toBe('Full name')
    expect(fieldLabel('instance 2025-03-17T09:00:00 Europe/Berlin summary')).toBe(
      'Summary (instance 2025-03-17T09:00:00 Europe/Berlin)',
    )
    expect(changeKind({ field: 'a', before: 'x', after: 'y' })).toBe('changed')
    expect(changeKind({ field: 'a', after: 'y' })).toBe('added')
    expect(changeKind({ field: 'a', before: 'x', after: null })).toBe('removed')
  })
})
