/** A response the server answered with an error status. */
export class HttpError extends Error {
  readonly status: number
  readonly body: string

  constructor(status: number, body: string) {
    super(`HTTP ${status}${body ? `: ${body}` : ''}`)
    this.status = status
    this.body = body
  }
}

/** Same-origin requests: the interface is served by the server it controls,
 * so the browser adds HTTP authentication by itself. */
export async function request(path: string, init?: RequestInit): Promise<Response> {
  const response = await fetch(path, { credentials: 'same-origin', ...init })
  if (!response.ok) {
    throw new HttpError(response.status, await response.text())
  }
  return response
}

export async function getText(path: string): Promise<string> {
  return (await request(path)).text()
}

export async function postJson<T>(path: string, body: unknown): Promise<T> {
  const response = await request(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  const text = await response.text()
  return (text ? JSON.parse(text) : undefined) as T
}

/** What to show a person: the server's `{"error": …}` text when there is one. */
export function errorText(error: unknown): string {
  if (error instanceof HttpError) {
    try {
      const parsed = JSON.parse(error.body) as { error?: unknown }
      if (typeof parsed.error === 'string' && parsed.error) return parsed.error
    } catch {
      // Not JSON: the body or the status is the message.
    }
    return error.body.trim() || `HTTP ${error.status}`
  }
  return error instanceof Error ? error.message : String(error)
}
