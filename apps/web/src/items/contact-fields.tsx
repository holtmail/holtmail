import { Button } from '@holtmail/ui'
import { PlusIcon, XIcon } from 'lucide-react'
import { useState } from 'react'

import {
  type AddressRow,
  addressLabels,
  type ContactDraft,
  emailLabels,
  emptyAddress,
  emptyLabeled,
  type LabeledRow,
  phoneLabels,
  withNamePart,
} from '../lib/edits.ts'
import { strings } from '../strings.ts'
import { Labeled, NativeSelect, SmallInput, TextArea } from './inputs.tsx'

const t = strings.edits.contact

export function ContactFields({
  draft,
  onChange,
}: {
  draft: ContactDraft
  onChange: (draft: ContactDraft) => void
}) {
  const set = <K extends keyof ContactDraft>(key: K, value: ContactDraft[K]) =>
    onChange({ ...draft, [key]: value })
  const [moreNames, setMoreNames] = useState(
    Boolean(draft.additional || draft.prefix || draft.suffix),
  )

  return (
    <div className="grid gap-4">
      <div className="grid gap-3 sm:grid-cols-2">
        <SmallInput
          label={t.given}
          value={draft.given}
          autoFocus
          onChange={(value) => onChange(withNamePart(draft, 'given', value))}
        />
        <SmallInput
          label={t.family}
          value={draft.family}
          onChange={(value) => onChange(withNamePart(draft, 'family', value))}
        />
        {moreNames && (
          <>
            <SmallInput
              label={t.additional}
              value={draft.additional}
              onChange={(value) => onChange(withNamePart(draft, 'additional', value))}
            />
            <SmallInput
              label={t.prefix}
              value={draft.prefix}
              onChange={(value) => onChange(withNamePart(draft, 'prefix', value))}
            />
            <SmallInput
              label={t.suffix}
              value={draft.suffix}
              onChange={(value) => onChange(withNamePart(draft, 'suffix', value))}
            />
          </>
        )}
      </div>
      {!moreNames && (
        <Button
          variant="ghost"
          size="sm"
          className="justify-self-start"
          onClick={() => setMoreNames(true)}
        >
          {t.moreNames}
        </Button>
      )}
      <SmallInput
        label={t.fullName}
        value={draft.fullName}
        onChange={(value) => set('fullName', value)}
      />
      <div className="grid gap-3 sm:grid-cols-2">
        <SmallInput
          label={t.organization}
          value={draft.organization}
          onChange={(value) => set('organization', value)}
        />
        <SmallInput label={t.title} value={draft.title} onChange={(value) => set('title', value)} />
      </div>
      <LabeledRows
        legend={t.emails}
        field={t.email}
        type="email"
        add={t.addEmail}
        labels={emailLabels}
        rows={draft.emails}
        onChange={(rows) => set('emails', rows)}
      />
      <LabeledRows
        legend={t.phones}
        field={t.phone}
        type="tel"
        add={t.addPhone}
        labels={phoneLabels}
        rows={draft.phones}
        onChange={(rows) => set('phones', rows)}
      />
      <fieldset className="grid gap-3">
        <legend className="mb-1 text-xs text-fg-muted">{t.addresses}</legend>
        {draft.addresses.map((address, index) => (
          <AddressFields
            key={index}
            address={address}
            onChange={(next) =>
              set(
                'addresses',
                draft.addresses.map((a, i) => (i === index ? next : a)),
              )
            }
            onRemove={() =>
              set(
                'addresses',
                draft.addresses.filter((_, i) => i !== index),
              )
            }
          />
        ))}
        <Button
          variant="ghost"
          size="sm"
          className="justify-self-start"
          onClick={() => set('addresses', [...draft.addresses, emptyAddress()])}
        >
          <PlusIcon />
          {t.addAddress}
        </Button>
      </fieldset>
      <Labeled label={t.notes}>
        {(id) => <TextArea id={id} value={draft.notes} onChange={(value) => set('notes', value)} />}
      </Labeled>
      <p className="text-xs text-fg-subtle">{t.photo}</p>
    </div>
  )
}

function labelChoices(labels: readonly string[]) {
  return labels.map((value) => ({ value, label: t.labels[value] ?? value }))
}

function LabeledRows({
  legend,
  field,
  type,
  add,
  labels,
  rows,
  onChange,
}: {
  legend: string
  field: string
  type: 'email' | 'tel'
  add: string
  labels: readonly string[]
  rows: LabeledRow[]
  onChange: (rows: LabeledRow[]) => void
}) {
  const update = (index: number, row: LabeledRow) =>
    onChange(rows.map((r, i) => (i === index ? row : r)))
  return (
    <fieldset className="grid gap-2">
      <legend className="mb-1 text-xs text-fg-muted">{legend}</legend>
      {rows.map((row, index) => (
        <div key={index} className="flex items-end gap-2">
          <SmallInput
            label={field}
            type={type}
            className="flex-1"
            value={row.value}
            spellCheck={false}
            onChange={(value) => update(index, { ...row, value })}
          />
          <NativeSelect
            label={t.label}
            className="w-32"
            value={row.label}
            choices={labelChoices(labels)}
            onChange={(label) => update(index, { ...row, label })}
          />
          <Button
            variant="ghost"
            size="icon"
            aria-label={t.remove}
            onClick={() => onChange(rows.filter((_, i) => i !== index))}
          >
            <XIcon />
          </Button>
        </div>
      ))}
      <Button
        variant="ghost"
        size="sm"
        className="justify-self-start"
        onClick={() => onChange([...rows, emptyLabeled()])}
      >
        <PlusIcon />
        {add}
      </Button>
    </fieldset>
  )
}

function AddressFields({
  address,
  onChange,
  onRemove,
}: {
  address: AddressRow
  onChange: (address: AddressRow) => void
  onRemove: () => void
}) {
  const set = <K extends keyof AddressRow>(key: K, value: AddressRow[K]) =>
    onChange({ ...address, [key]: value })
  return (
    <div className="grid gap-2 border-l border-border pl-3">
      <div className="flex items-end gap-2">
        <NativeSelect
          label={t.label}
          className="w-32"
          value={address.label}
          choices={labelChoices(addressLabels)}
          onChange={(label) => set('label', label)}
        />
        <span className="flex-1" />
        <Button variant="ghost" size="icon" aria-label={t.remove} onClick={onRemove}>
          <XIcon />
        </Button>
      </div>
      <SmallInput label={t.street} value={address.street} onChange={(v) => set('street', v)} />
      <SmallInput
        label={t.extended}
        value={address.extended}
        onChange={(v) => set('extended', v)}
      />
      <div className="grid gap-2 sm:grid-cols-3">
        <SmallInput
          label={t.postalCode}
          value={address.postalCode}
          onChange={(v) => set('postalCode', v)}
        />
        <SmallInput
          label={t.locality}
          value={address.locality}
          onChange={(v) => set('locality', v)}
        />
        <SmallInput label={t.region} value={address.region} onChange={(v) => set('region', v)} />
      </div>
      <div className="grid gap-2 sm:grid-cols-2">
        <SmallInput label={t.country} value={address.country} onChange={(v) => set('country', v)} />
        <SmallInput label={t.poBox} value={address.poBox} onChange={(v) => set('poBox', v)} />
      </div>
    </div>
  )
}
