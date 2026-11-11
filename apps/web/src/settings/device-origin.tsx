import { devicesQuery } from '@holtmail/api-client'
import { useQuery } from '@tanstack/react-query'

import { changeDevice } from '../lib/devices.ts'
import { strings } from '../strings.ts'

/** Which device a change came from, for changes a mail, calendar or contacts app made. */
export function DeviceOrigin({ deviceId }: { deviceId: string | null | undefined }) {
  const devices = useQuery({ ...devicesQuery, enabled: deviceId != null }).data
  const name = changeDevice(deviceId, devices)
  if (name === undefined) return null
  const o = strings.devices.origin
  return <span>{name === null ? o.unknown : o.named(name)}</span>
}
