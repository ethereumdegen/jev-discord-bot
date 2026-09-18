export class ApiError extends Error {
  constructor(public status: number, public code: string, message: string) {
    super(message)
  }
}

function cookie(name: string): string | undefined {
  return document.cookie
    .split(';')
    .map((part) => part.trim())
    .find((part) => part.startsWith(`${name}=`))
    ?.slice(name.length + 1)
}

/** Call the API. Writes carry the CSRF token from its cookie. */
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  if (init.body && !headers.has('content-type')) headers.set('content-type', 'application/json')
  const method = (init.method ?? 'GET').toUpperCase()
  const csrf = cookie('dg_csrf')
  if (csrf && method !== 'GET') headers.set('x-csrf-token', decodeURIComponent(csrf))
  const response = await fetch(`/api${path}`, { ...init, headers, credentials: 'same-origin' })
  if (!response.ok) {
    let body: { error?: { code?: string; message?: string } } | undefined
    try {
      body = await response.json()
    } catch {
      /* not JSON */
    }
    throw new ApiError(response.status, body?.error?.code ?? 'request_failed', body?.error?.message ?? `Request failed (${response.status})`)
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}

export const send = <T>(method: string, path: string, body?: unknown) => api<T>(path, { method, body: body === undefined ? undefined : JSON.stringify(body) })

export function dateTime(value: string | null | undefined): string {
  if (!value) return ''
  return new Date(value).toLocaleString('en-US', { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })
}

export const percent = (value: number) => `${Math.round(value * 100)}%`

export const number = (value: number) => new Intl.NumberFormat('en-US').format(value)
