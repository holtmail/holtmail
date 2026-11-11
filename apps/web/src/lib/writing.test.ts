import { describe, expect, test } from 'bun:test'

import { ApiError, type MessageDetail } from '@holtmail/api-client'

import {
  archivedAttachment,
  canApplyAnswers,
  canCompose,
  discardsOnClose,
  capabilitySwitch,
  editAsNewPayload,
  fromLocalInput,
  grantableRoles,
  htmlToText,
  insertTextSignature,
  mayChangeGrant,
  readerActions,
  renewsSendKey,
  roleIncludes,
  signatureHtml,
  signatureText,
  splitQuote,
  textToHtml,
  toggleCapability,
  toLocalInput,
} from './writing.ts'

describe('gating', () => {
  test('compose shows only with the right to compose', () => {
    expect(canCompose(undefined)).toBe(false)
    expect(canCompose({ rights: { role: 'sender', manage: false, compose: false } })).toBe(false)
    expect(canCompose({ rights: { role: 'sender', manage: false, compose: true } })).toBe(true)
  })

  test('apply anyway shows to editors and senders only', () => {
    expect(canApplyAnswers(undefined)).toBe(false)
    expect(canApplyAnswers({ rights: { role: 'reader', manage: false, compose: false } })).toBe(
      false,
    )
    expect(canApplyAnswers({ rights: { role: 'editor', manage: false, compose: false } })).toBe(
      true,
    )
    expect(canApplyAnswers({ rights: { role: 'sender', manage: true, compose: true } })).toBe(true)
  })

  test('reader actions need compose, and a list for Reply to list', () => {
    expect(readerActions({ list_post: 'dev@lists.example.org' }, false)).toEqual([])
    expect(readerActions(undefined, true)).toEqual([])
    expect(readerActions({ list_post: null }, true)).toEqual([
      'reply',
      'replyAll',
      'forward',
      'forwardAttachment',
      'editAsNew',
    ])
    expect(readerActions({ list_post: 'dev@lists.example.org' }, true)).toContain('replyList')
  })

  test('roles include the ones before them', () => {
    expect(roleIncludes('sender', 'editor')).toBe(true)
    expect(roleIncludes('reader', 'editor')).toBe(false)
    expect(roleIncludes(undefined, 'reader')).toBe(false)
    expect(roleIncludes('owner', 'reader')).toBe(false)
  })

  test('only the owner hands out or takes away writing grants', () => {
    expect(grantableRoles('owner')).toEqual(['reader', 'editor', 'sender'])
    expect(grantableRoles('admin')).toEqual(['reader'])
    expect(mayChangeGrant('admin', 'reader')).toBe(true)
    expect(mayChangeGrant('admin', undefined)).toBe(true)
    expect(mayChangeGrant('admin', 'sender')).toBe(false)
    expect(mayChangeGrant('owner', 'sender')).toBe(true)
  })
})

describe('capability switches', () => {
  test('expunge needs delete', () => {
    expect(toggleCapability([], 'mail.expunge', true)).toEqual([])
    expect(toggleCapability(['mail.delete'], 'mail.expunge', true).toSorted()).toEqual([
      'mail.delete',
      'mail.expunge',
    ])
    expect(
      toggleCapability(['mail.delete', 'mail.expunge', 'mail.send'], 'mail.delete', false),
    ).toEqual(['mail.send'])
    expect(capabilitySwitch({ capabilities: [], active: [] }, 'mail.expunge').blocked).toBe(true)
    expect(capabilitySwitch({ capabilities: ['mail.send'], active: [] }, 'mail.send')).toEqual({
      on: true,
      effective: false,
      blocked: false,
    })
  })
})

describe('compose helpers', () => {
  test('splits the editable part from the quoted original', () => {
    const html =
      '<p>Hi</p>\n<div class="holtmail-quote"><p>Ada wrote:</p><blockquote>x</blockquote></div>'
    expect(splitQuote(html)).toEqual({
      own: '<p>Hi</p>\n',
      quote: '<div class="holtmail-quote"><p>Ada wrote:</p><blockquote>x</blockquote></div>',
    })
    expect(splitQuote('<p>new</p>')).toEqual({ own: '<p>new</p>', quote: '' })
    expect(splitQuote(null)).toEqual({ own: '', quote: '' })
    expect(splitQuote('<div class="holtmail-forward">f</div>').own).toBe('')
  })

  test('moves between text and HTML', () => {
    expect(textToHtml('a <b>\nc\n\nd')).toBe('<p>a &lt;b&gt;<br>c</p><p>d</p>')
    expect(htmlToText('<p>a &amp; b</p><ul><li>one</li><li>two</li></ul>')).toBe(
      'a & b\n\n- one\n\n- two',
    )
  })

  test('places the signature before the quote', () => {
    const identity = { signature_text: 'Ada', signature_html: null }
    expect(signatureText(identity)).toBe('-- \nAda')
    expect(signatureText({ signature_text: null, signature_html: '<p>Ada <b>L</b></p>' })).toBe(
      '-- \nAda L',
    )
    expect(signatureText({ signature_text: '', signature_html: null })).toBe('')
    expect(signatureHtml(identity)).toBe('<p>-- </p><p>Ada</p>')
    expect(insertTextSignature('Hello', '-- \nAda')).toBe('Hello\n\n-- \nAda\n')
    expect(insertTextSignature('Hello\n\nOn Mon, Ada wrote:\n> x\n', '-- \nAda')).toBe(
      'Hello\n\n-- \nAda\n\nOn Mon, Ada wrote:\n> x\n',
    )
    expect(insertTextSignature('Hello\n\n-- \nAda\n', '-- \nAda')).toBe('Hello\n\n-- \nAda\n')
  })

  test('edit as new keeps recipients, subject, text and files', () => {
    const message = {
      id: 'm1',
      to: [{ name: 'Bob', email: 'bob@example.org' }],
      cc: [{ name: null, email: 'cy@example.net' }],
      subject: 'Plans',
      text: 'Body',
      attachments: [
        { ordinal: 1, filename: 'a.pdf', content_type: 'application/pdf', size: 3, inline: false },
        { ordinal: 2, filename: 'logo.png', content_type: 'image/png', size: 1, inline: true },
      ],
    } as unknown as MessageDetail
    const payload = editAsNewPayload(message)
    expect(payload.to).toEqual([{ name: 'Bob', address: 'bob@example.org' }])
    expect(payload.cc).toEqual([{ address: 'cy@example.net' }])
    expect(payload.subject).toBe('Plans')
    expect(payload.attachments).toEqual([
      { message: 'm1', ordinal: 1, name: 'a.pdf', content_type: 'application/pdf', size: 3 },
    ])
    expect(payload.context).toEqual({ kind: 'new', message: null })
  })

  test('schedule times round trip through the local input', () => {
    const time = new Date(2026, 10, 7, 9, 5).getTime()
    expect(toLocalInput(time)).toBe('2026-11-07T09:05')
    expect(fromLocalInput('2026-11-07T09:05')).toBe(time)
    expect(fromLocalInput('')).toBeUndefined()
  })
})

describe('sending from a compose window', () => {
  test('a definite refusal renews the Idempotency-Key, a lost answer keeps it', () => {
    expect(renewsSendKey(new ApiError(400, 'no route'))).toBe(true)
    expect(renewsSendKey(new ApiError(403, 'no grant'))).toBe(true)
    expect(renewsSendKey(new ApiError(422, 'another body'))).toBe(true)
    expect(renewsSendKey(new ApiError(502, 'bad gateway'))).toBe(false)
    expect(renewsSendKey(new TypeError('Failed to fetch'))).toBe(false)
  })

  test('closing deletes only untouched fresh drafts and empty new ones', () => {
    const reply = {
      to: [{ address: 'bob@example.org' }],
      subject: 'Re: Plans',
      text: '> Plans',
      context: { kind: 'reply', message: 'm1' },
    }
    expect(discardsOnClose(reply, { fresh: true, touched: false })).toBe(true)
    expect(discardsOnClose(reply, { fresh: true, touched: true })).toBe(false)
    expect(discardsOnClose(reply, { fresh: false, touched: false })).toBe(false)
    expect(discardsOnClose({ context: { kind: 'new' } }, { fresh: false, touched: true })).toBe(
      true,
    )
  })

  test('an archived attachment points at its message and ordinal', () => {
    expect(
      archivedAttachment('m1', {
        ordinal: 2,
        filename: 'plans.pdf',
        content_type: 'application/pdf',
        size: 1200,
      }),
    ).toEqual({
      message: 'm1',
      ordinal: 2,
      name: 'plans.pdf',
      content_type: 'application/pdf',
      size: 1200,
    })
  })
})
