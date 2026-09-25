import { getText } from './http'

/** `GET /echo`: the version string clients recognise the server by. */
export function serverVersion(): Promise<string> {
  return getText('/echo')
}
