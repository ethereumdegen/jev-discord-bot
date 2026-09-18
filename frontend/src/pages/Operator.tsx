import { useState } from 'react'
import { number } from '../api/client'
import { FormError, Query, RequireAccount } from '../components/ui'
import { useAction, useApi, useMe } from '../hooks/api'

interface Row {
  id: string
  name: string
  mode: string
  allowance: number
  judged: number
  removed_at: string | null
}

export function OperatorPage() {
  const { account } = useMe()
  const data = useApi<{ servers: Row[] }>(account?.is_operator ? '/operator/servers' : null)
  return (
    <RequireAccount>
      {!account?.is_operator ? (
        <p className="muted">Operators only.</p>
      ) : (
        <div className="stack">
          <h1 className="prompt">operator</h1>
          <p className="muted small">Every server with the bot. Jev calls are on you: raise a server's monthly allowance here.</p>
          <Query query={data}>
            {(d) => (
              <div className="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Server</th>
                      <th>Mode</th>
                      <th>This month</th>
                      <th>Allowance</th>
                    </tr>
                  </thead>
                  <tbody>
                    {d.servers.map((s) => (
                      <tr key={s.id}>
                        <td>
                          {s.name}
                          {s.removed_at && <span className="faint tiny"> (removed)</span>}
                        </td>
                        <td className="small">{s.mode}</td>
                        <td className="small">{number(s.judged)}</td>
                        <td>
                          <Allowance row={s} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Query>
        </div>
      )}
    </RequireAccount>
  )
}

function Allowance({ row }: { row: Row }) {
  const [value, setValue] = useState(String(row.allowance))
  const save = useAction<void>('PATCH', `/operator/servers/${row.id}`, { body: () => ({ monthly_allowance: Number(value) }) })
  return (
    <div className="row">
      <input className="short" inputMode="numeric" value={value} onChange={(e) => setValue(e.target.value)} aria-label={`Allowance for ${row.name}`} />
      <button className="button small" type="button" disabled={value === String(row.allowance) || save.isPending} onClick={() => save.mutate()}>
        Save
      </button>
      <FormError error={save.error} />
    </div>
  )
}
