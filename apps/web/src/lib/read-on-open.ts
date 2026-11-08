export const readOnOpenKey = 'holtmail.read-on-open'

/** The stored choice: on only when it says so, so a missing or unreadable value keeps reading
 * in Holtmail from changing anything on the server. */
export function parseReadOnOpen(value: string | null | undefined): boolean {
  return value === 'on'
}
