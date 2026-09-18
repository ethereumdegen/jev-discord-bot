import type { ReactNode } from 'react'
import { Link, NavLink } from 'react-router-dom'
import { useAction, useMe } from '../hooks/api'

export function Loading() {
  return (
    <div className="state" role="status">
      <span className="spinner" /> <span className="mono small">Loading…</span>
    </div>
  )
}

export function Query<T>({ query, children }: { query: { data?: T; error: unknown; isLoading: boolean }; children: (data: T) => ReactNode }) {
  if (query.error) return <div className="state error">{query.error instanceof Error ? query.error.message : 'Something went wrong.'}</div>
  if (query.isLoading || query.data === undefined) return <Loading />
  return <>{children(query.data)}</>
}

export function Badge({ tone, children }: { tone?: 'accent' | 'ok' | 'warn' | 'danger'; children: ReactNode }) {
  return <span className={`badge${tone ? ` ${tone}` : ''}`}>{children}</span>
}

export function FormError({ error }: { error: Error | null | undefined }) {
  return error ? <p className="error small">{error.message}</p> : null
}

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="field">
      {label}
      {children}
      {hint && <span className="faint tiny">{hint}</span>}
    </label>
  )
}

export function Stat({ value, label }: { value: ReactNode; label: string }) {
  return (
    <div className="stat">
      <b>{value}</b>
      <span>{label}</span>
    </div>
  )
}

/** Sign-in buttons; Discord first, since that's where the servers are. */
export function SignIn({ returnTo = '/servers' }: { returnTo?: string }) {
  const { site } = useMe()
  const q = `return_to=${encodeURIComponent(returnTo)}`
  return (
    <div className="row">
      <a className="button primary" href={`/api/auth/discord/start?${q}`}>
        Continue with Discord
      </a>
      {site?.google && (
        <a className="button" href={`/api/auth/google/start?${q}`}>
          Continue with Google
        </a>
      )}
    </div>
  )
}

export function Shell({ children }: { children: ReactNode }) {
  const { account } = useMe()
  const logout = useAction<void>('POST', '/auth/logout', { onSuccess: () => window.location.assign('/') })
  return (
    <>
      <header className="site-header">
        <div className="page">
          <Link to="/" className="logo">
            <span>&gt;_</span> degen guard
          </Link>
          <nav className="nav" aria-label="Main">
            {account && <NavLink to="/servers">Your servers</NavLink>}
            {account?.is_operator && <NavLink to="/operator">Operator</NavLink>}
          </nav>
          {account ? (
            <div className="row">
              <span className="small muted">{account.discord_username ? `@${account.discord_username}` : account.name}</span>
              <button className="button small ghost" type="button" onClick={() => logout.mutate()}>
                Sign out
              </button>
            </div>
          ) : (
            <a className="button small primary" href="/api/auth/discord/start?return_to=/servers">
              Sign in
            </a>
          )}
        </div>
      </header>
      <main className="page">{children}</main>
      <footer className="site-footer">
        <div className="page row between">
          <span className="mono">&gt;_ degen guard · judged by Jev</span>
          <a href="https://degenbuilders.com">degenbuilders.com</a>
        </div>
      </footer>
    </>
  )
}

export function RequireAccount({ children }: { children: ReactNode }) {
  const { account, isLoading } = useMe()
  if (isLoading) return <Loading />
  if (!account)
    return (
      <div className="stack narrow">
        <h1 className="prompt">sign in</h1>
        <p className="muted">Sign in to add Degen Guard to your servers and see what it caught.</p>
        <SignIn />
      </div>
    )
  return <>{children}</>
}

export const avatar = (id: string, icon: string | null) => (icon ? `https://cdn.discordapp.com/icons/${id}/${icon}.png?size=96` : null)

export function ServerIcon({ id, icon, name }: { id: string; icon: string | null; name: string }) {
  const src = avatar(id, icon)
  return <div className="artwork server-icon">{src ? <img src={src} alt="" /> : name.slice(0, 1).toUpperCase()}</div>
}
