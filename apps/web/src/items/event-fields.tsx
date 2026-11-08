import { Button, Switch } from '@holtmail/ui'
import { PlusIcon, XIcon } from 'lucide-react'
import { useId, useMemo } from 'react'

import {
  alarmChoices,
  type EventDraft,
  isRepeat,
  repeats,
  withStart,
  zoneChoices,
} from '../lib/edits.ts'
import { participantLabel, viewerZone } from '../lib/events.ts'
import { strings } from '../strings.ts'
import { Labeled, NativeSelect, SmallInput, TextArea } from './inputs.tsx'

const t = strings.edits

export function EventFields({
  draft,
  onChange,
}: {
  draft: EventDraft
  onChange: (draft: EventDraft) => void
}) {
  const set = <K extends keyof EventDraft>(key: K, value: EventDraft[K]) =>
    onChange({ ...draft, [key]: value })
  const zones = useMemo(() => zoneChoices(viewerZone()), [])
  const zoneList = useId()
  const allDayId = useId()
  const unusedAlarm = alarmChoices.find((m) => !draft.alarms.includes(m))

  return (
    <div className="grid gap-4">
      <SmallInput
        label={t.event.title}
        value={draft.title}
        autoFocus
        onChange={(value) => set('title', value)}
      />
      <div className="flex items-center gap-3">
        <Switch id={allDayId} checked={draft.allDay} onCheckedChange={(on) => set('allDay', on)} />
        <label htmlFor={allDayId} className="text-sm">
          {t.event.allDay}
        </label>
      </div>
      <fieldset className="grid gap-2">
        <legend className="mb-2 text-xs text-fg-muted">{t.event.starts}</legend>
        <div className="flex flex-wrap gap-2">
          <SmallInput
            label={t.event.date}
            type="date"
            className="w-40"
            value={draft.startDate}
            onChange={(value) => onChange(withStart(draft, value, draft.startTime))}
          />
          {!draft.allDay && (
            <SmallInput
              label={t.event.time}
              type="time"
              className="w-28"
              value={draft.startTime}
              onChange={(value) => onChange(withStart(draft, draft.startDate, value))}
            />
          )}
        </div>
      </fieldset>
      <fieldset className="grid gap-2">
        <legend className="mb-2 text-xs text-fg-muted">{t.event.ends}</legend>
        <div className="flex flex-wrap gap-2">
          <SmallInput
            label={t.event.date}
            type="date"
            className="w-40"
            value={draft.endDate}
            onChange={(value) => set('endDate', value)}
          />
          {!draft.allDay && (
            <SmallInput
              label={t.event.time}
              type="time"
              className="w-28"
              value={draft.endTime}
              onChange={(value) => set('endTime', value)}
            />
          )}
        </div>
      </fieldset>
      {!draft.allDay && (
        <div className="grid gap-1">
          <SmallInput
            label={t.event.timeZone}
            list={zoneList}
            className="max-w-sm"
            value={draft.timeZone}
            spellCheck={false}
            onChange={(value) => set('timeZone', value)}
          />
          <datalist id={zoneList}>
            {zones.map((zone) => (
              <option key={zone} value={zone} />
            ))}
          </datalist>
          <span className="text-xs text-fg-subtle">{t.event.floating}</span>
        </div>
      )}
      <NativeSelect
        label={t.event.repeat}
        className="max-w-sm"
        value={draft.repeat}
        choices={repeats.map((r) => ({ value: r, label: t.event.repeats[r] }))}
        onChange={(value) => {
          if (isRepeat(value)) set('repeat', value)
        }}
      />
      {draft.repeat === 'custom' && (
        <div className="grid gap-1">
          <SmallInput
            label={t.event.rule}
            className="font-mono"
            value={draft.rule}
            spellCheck={false}
            onChange={(value) => set('rule', value)}
          />
          <span className="text-xs text-fg-subtle">{t.event.ruleHint}</span>
        </div>
      )}
      <SmallInput
        label={t.event.location}
        value={draft.location}
        onChange={(value) => set('location', value)}
      />
      <Labeled label={t.event.description}>
        {(id) => (
          <TextArea
            id={id}
            value={draft.description}
            onChange={(value) => set('description', value)}
          />
        )}
      </Labeled>
      <fieldset className="grid gap-2">
        <legend className="mb-1 text-xs text-fg-muted">{t.event.invitees}</legend>
        {draft.organizer && (
          <p className="text-sm text-fg-muted">
            {t.event.organizer(
              participantLabel({
                email: draft.organizer.email,
                name: draft.organizer.name ?? undefined,
              }),
            )}
          </p>
        )}
        {draft.attendees.map((attendee, index) => (
          <div key={index} className="flex items-end gap-2">
            <SmallInput
              label={t.event.inviteeEmail}
              type="email"
              className="flex-1"
              value={attendee.email}
              onChange={(value) =>
                set(
                  'attendees',
                  draft.attendees.map((a, i) => (i === index ? { ...a, email: value } : a)),
                )
              }
            />
            <SmallInput
              label={t.event.inviteeName}
              className="flex-1"
              value={attendee.name ?? ''}
              onChange={(value) =>
                set(
                  'attendees',
                  draft.attendees.map((a, i) => (i === index ? { ...a, name: value } : a)),
                )
              }
            />
            <Button
              variant="ghost"
              size="icon"
              aria-label={t.event.remove}
              onClick={() =>
                set(
                  'attendees',
                  draft.attendees.filter((_, i) => i !== index),
                )
              }
            >
              <XIcon />
            </Button>
          </div>
        ))}
        <Button
          variant="ghost"
          size="sm"
          className="justify-self-start"
          onClick={() =>
            set('attendees', [...draft.attendees, { email: '', name: null, partstat: null }])
          }
        >
          <PlusIcon />
          {t.event.addInvitee}
        </Button>
        <span className="text-xs text-fg-subtle">{t.event.inviteesHint}</span>
      </fieldset>
      <fieldset className="grid gap-2">
        <legend className="mb-1 text-xs text-fg-muted">{t.event.alarms}</legend>
        {draft.alarms.map((minutes, index) => (
          <div key={index} className="flex items-end gap-2">
            <NativeSelect
              label={t.event.alarms}
              hideLabel
              className="w-56"
              value={String(minutes)}
              choices={[...new Set([minutes, ...alarmChoices])]
                .toSorted((a, b) => a - b)
                .map((m) => ({ value: String(m), label: t.event.alarm(m) }))}
              onChange={(value) =>
                set(
                  'alarms',
                  draft.alarms.map((m, i) => (i === index ? Number(value) : m)),
                )
              }
            />
            <Button
              variant="ghost"
              size="icon"
              aria-label={t.event.remove}
              onClick={() =>
                set(
                  'alarms',
                  draft.alarms.filter((_, i) => i !== index),
                )
              }
            >
              <XIcon />
            </Button>
          </div>
        ))}
        {unusedAlarm !== undefined && (
          <Button
            variant="ghost"
            size="sm"
            className="justify-self-start"
            onClick={() => set('alarms', [...draft.alarms, unusedAlarm])}
          >
            <PlusIcon />
            {t.event.addAlarm}
          </Button>
        )}
      </fieldset>
    </div>
  )
}
