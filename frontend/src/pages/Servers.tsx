import { Link, useSearchParams } from 'react-router-dom'
import type { ServerCard } from '../api/types'
import { Badge, Query, RequireAccount, ServerIcon } from '../components/ui'
import { useApi } from '../hooks/api'

const ERRORS: Record<string, string> = {
  not_manager: "You need Manage Server in that server to add the bot to it.",
  discord: "Linking Discord didn't finish. Try again.",
}

export function ServersPage() {
  const data = useApi<{ servers: ServerCard[]; discord_connected: boolean }>('/servers')
  const [params] = useSearchParams()
  const error = params.get('error')
  return (
    <div className="stack">
      {/* A Discord round trip that failed can leave you signed out: say so
          here rather than behind the sign-in screen, where nobody sees it. */}
      {error && <p className="notice danger small">{ERRORS[error] ?? 'Something went wrong.'}</p>}
      {params.get('install') === 'cancelled' && <p className="notice warn small">The bot wasn't added.</p>}
      <RequireAccount>
        <div className="stack">
          <div className="row between">
            <h1 className="prompt">your servers</h1>
            <a className="button primary" href="/api/auth/discord/start?purpose=install">
              Add to a server
            </a>
          </div>
          <Query query={data}>
            {(d) =>
              !d.discord_connected ? (
                <div className="card stack">
                  <p>Connect Discord so we can see which servers you manage.</p>
                  <a className="button primary" href="/api/auth/discord/start?return_to=/servers">
                    Connect Discord
                  </a>
                </div>
              ) : d.servers.length === 0 ? (
                <p className="muted">You don't manage any servers. You need Manage Server (or to own the server) to add the bot.</p>
              ) : (
                <div className="grid two">
                  {d.servers.map((s) => (
                    <div key={s.id} className="card server-card">
                      <ServerIcon id={s.id} icon={s.icon} name={s.name} />
                      <div className="stack">
                        <div className="row between">
                          <h3>{s.name}</h3>
                          {s.installed && <Badge tone={s.mode === 'enforce' ? 'accent' : undefined}>{s.mode}</Badge>}
                        </div>
                        {s.installed ? (
                          <Link className="button small" to={`/servers/${s.id}`}>
                            Settings and log
                          </Link>
                        ) : (
                          <a className="button small ghost" href="/api/auth/discord/start?purpose=install">
                            Add the bot
                          </a>
                        )}
                      </div>
                    </div>
                  ))}
                </div>
              )
            }
          </Query>
          <p className="faint tiny">
            Missing a server? <a href="/api/auth/discord/start?return_to=/servers">Refresh the list from Discord</a>.
          </p>
        </div>
      </RequireAccount>
    </div>
  )
}
