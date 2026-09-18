import { useEffect, useState } from 'react'
import { Link, NavLink, useParams, useSearchParams } from 'react-router-dom'
import { api, dateTime, number, percent } from '../api/client'
import type { Action, Named, Rule, ServerPage, Step } from '../api/types'
import { Badge, Field, FormError, Query, RequireAccount, ServerIcon, Stat } from '../components/ui'
import { useAction, useApi } from '../hooks/api'

const STEPS: Step[] = ['none', 'warn', 'timeout', 'kick', 'ban']

export function ServerLayout({ tab }: { tab: 'overview' | 'settings' | 'log' }) {
  const { id = '' } = useParams()
  const [params] = useSearchParams()
  const page = useApi<ServerPage>(`/servers/${id}`)
  return (
    <RequireAccount>
      <Query query={page}>
        {(p) => (
          <div className="stack">
            {params.get('installed') && <p className="notice small">Added! Degen Guard is watching: it logs what it would do and touches nothing until you switch it to enforce.</p>}
            <div className="row">
              <ServerIcon id={p.server.id} icon={p.server.icon} name={p.server.name} />
              <div>
                <h1>{p.server.name}</h1>
                <ModeBadge mode={p.server.mode} />
              </div>
            </div>
            <nav className="tabs" aria-label="Server">
              <NavLink to={`/servers/${id}`} end>Overview</NavLink>
              <NavLink to={`/servers/${id}/settings`}>Settings</NavLink>
              <NavLink to={`/servers/${id}/log`}>Audit log</NavLink>
            </nav>
            {tab === 'overview' && <Overview page={p} />}
            {tab === 'settings' && <Settings page={p} />}
            {tab === 'log' && <Log serverId={id} />}
          </div>
        )}
      </Query>
    </RequireAccount>
  )
}

function ModeBadge({ mode }: { mode: string }) {
  return <Badge tone={mode === 'enforce' ? 'accent' : mode === 'paused' ? 'warn' : undefined}>{mode === 'watch' ? 'watching' : mode === 'enforce' ? 'enforcing' : 'paused'}</Badge>
}

function Overview({ page }: { page: ServerPage }) {
  const { server, month } = page
  const setMode = useAction<string>('PATCH', `/servers/${server.id}`, { body: (mode) => ({ mode }) })
  const left = Math.max(0, month.allowance - month.judged)
  return (
    <div className="stack">
      <div className="card stack">
        <h3>Mode</h3>
        <p className="muted small">Watch: log what it would do. Enforce: warn, kick and ban on your ladder. Paused: ignore messages.</p>
        <div className="row">
          {(['watch', 'enforce', 'paused'] as const).map((mode) => (
            <button key={mode} type="button" className={`button small${server.mode === mode ? ' primary' : ''}`} disabled={setMode.isPending} onClick={() => setMode.mutate(mode)}>
              {mode}
            </button>
          ))}
        </div>
        <FormError error={setMode.error} />
      </div>
      <h3>This month</h3>
      <div className="stats">
        <Stat value={`${number(month.judged)} / ${number(month.allowance)}`} label={left === 0 ? 'judged: allowance used up' : 'messages judged'} />
        <Stat value={month.actions.strikes} label="strikes" />
        <Stat value={month.actions.warns} label="warnings" />
        <Stat value={month.actions.kicks} label="kicks" />
        <Stat value={month.actions.bans} label="bans" />
        <Stat value={month.actions.review} label="waiting for review" />
        <Stat value={month.actions.undone} label="undone" />
        <Stat value={month.actions.failed} label="couldn't act" />
      </div>
      {month.actions.review > 0 && (
        <Link className="button" to={`/servers/${server.id}/log?filter=review`}>
          Review {month.actions.review} flagged messages
        </Link>
      )}
      {!server.log_channel_id && <p className="notice small">Tip: pick a log channel in Settings to see each call in Discord, with Undo buttons.</p>}
    </div>
  )
}

function Settings({ page }: { page: ServerPage }) {
  const { server } = page
  const discord = useApi<{ channels: Named[]; roles: Named[] }>(`/servers/${server.id}/discord`)
  const [draft, setDraft] = useState(server)
  useEffect(() => setDraft(server), [server])
  const save = useAction<void>('PATCH', `/servers/${server.id}`, {
    body: () => ({
      log_channel_id: draft.log_channel_id ?? '',
      exempt_role_ids: draft.exempt_role_ids,
      exempt_channel_ids: draft.exempt_channel_ids,
      confident_percent: Number(draft.confident_percent),
      flag_percent: Number(draft.flag_percent),
      strike_days: Number(draft.strike_days),
      timeout_minutes: Number(draft.timeout_minutes),
      community: draft.community,
    }),
  })
  const toggle = (list: string[], id: string) => (list.includes(id) ? list.filter((x) => x !== id) : [...list, id])
  const spam = page.rules.find((r) => r.kind === 'spam')
  return (
    <div className="stack">
      {spam && <LadderEditor serverId={server.id} rule={spam} />}
      <form
        className="card stack"
        onSubmit={(e) => {
          e.preventDefault()
          save.mutate()
        }}
      >
        <h3>How sure, and for how long</h3>
        <div className="grid">
          <Field label="Act when Jev is at least (%) sure" hint="A confirmed offense: a strike on the ladder.">
            <input type="number" min={1} max={100} value={draft.confident_percent} onChange={(e) => setDraft({ ...draft, confident_percent: Number(e.target.value) })} />
          </Field>
          <Field label="Flag for review from (%)" hint="Listed for you to decide; no strike.">
            <input type="number" min={1} max={100} value={draft.flag_percent} onChange={(e) => setDraft({ ...draft, flag_percent: Number(e.target.value) })} />
          </Field>
          <Field label="Strikes wear off after (days)">
            <input type="number" min={1} max={3650} value={draft.strike_days} onChange={(e) => setDraft({ ...draft, strike_days: Number(e.target.value) })} />
          </Field>
          <Field label="A time-out lasts (minutes)">
            <input type="number" min={1} max={40320} value={draft.timeout_minutes} onChange={(e) => setDraft({ ...draft, timeout_minutes: Number(e.target.value) })} />
          </Field>
        </div>
        <Field label="What's normal in your server" hint="Given to Jev with every message. E.g. 'An AI dev community; sharing your own projects is fine in #show-and-tell.'">
          <textarea maxLength={1000} value={draft.community} onChange={(e) => setDraft({ ...draft, community: e.target.value })} />
        </Field>
        <Query query={discord}>
          {(d) => (
            <>
              <Field label="Log channel" hint="Each call is posted here with Undo buttons. Make it mods-only.">
                <select value={draft.log_channel_id ?? ''} onChange={(e) => setDraft({ ...draft, log_channel_id: e.target.value || null })}>
                  <option value="">None</option>
                  {d.channels.map((c) => (
                    <option key={c.id} value={c.id}>#{c.name}</option>
                  ))}
                </select>
              </Field>
              <fieldset className="checks">
                <legend>Never judge these roles</legend>
                {d.roles.map((r) => (
                  <label key={r.id}>
                    <input type="checkbox" checked={draft.exempt_role_ids.includes(r.id)} onChange={() => setDraft({ ...draft, exempt_role_ids: toggle(draft.exempt_role_ids, r.id) })} /> {r.name}
                  </label>
                ))}
              </fieldset>
              <fieldset className="checks">
                <legend>Never judge these channels</legend>
                {d.channels.map((c) => (
                  <label key={c.id}>
                    <input type="checkbox" checked={draft.exempt_channel_ids.includes(c.id)} onChange={() => setDraft({ ...draft, exempt_channel_ids: toggle(draft.exempt_channel_ids, c.id) })} /> #{c.name}
                  </label>
                ))}
              </fieldset>
            </>
          )}
        </Query>
        <div className="row">
          <button className="button primary" type="submit" disabled={save.isPending}>
            Save
          </button>
          {save.isSuccess && <span className="small muted">Saved.</span>}
        </div>
        <FormError error={save.error} />
      </form>
    </div>
  )
}

function LadderEditor({ serverId, rule }: { serverId: string; rule: Rule }) {
  const [ladder, setLadder] = useState<Step[]>(rule.ladder)
  useEffect(() => setLadder(rule.ladder), [rule.ladder])
  const save = useAction<{ ladder?: Step[]; enabled?: boolean }>('PATCH', `/servers/${serverId}/rules/${rule.id}`)
  const changed = ladder.join() !== rule.ladder.join()
  return (
    <div className="card stack">
      <div className="row between">
        <h3>Rule: no spam</h3>
        <label className="row small">
          <input type="checkbox" checked={rule.enabled} onChange={(e) => save.mutate({ enabled: e.target.checked })} /> on
        </label>
      </div>
      <p className="muted small">Spam, scams and phishing, and off-topic self-promotion. What each confirmed offense does:</p>
      <ol className="ladder">
        {ladder.map((step, index) => (
          <li key={index} className="row">
            <span className="mono small">offense {index + 1}{index === ladder.length - 1 ? '+' : ''}</span>
            <select value={step} onChange={(e) => setLadder(ladder.map((s, i) => (i === index ? (e.target.value as Step) : s)))} aria-label={`Offense ${index + 1}`}>
              {STEPS.map((s) => (
                <option key={s} value={s}>{s}</option>
              ))}
            </select>
            {ladder.length > 1 && (
              <button className="button small ghost" type="button" onClick={() => setLadder(ladder.filter((_, i) => i !== index))}>
                Remove
              </button>
            )}
          </li>
        ))}
      </ol>
      <div className="row">
        {ladder.length < 10 && (
          <button className="button small ghost" type="button" onClick={() => setLadder([...ladder, 'ban'])}>
            Add a step
          </button>
        )}
        <button className="button small primary" type="button" disabled={!changed || save.isPending} onClick={() => save.mutate({ ladder })}>
          Save ladder
        </button>
      </div>
      <FormError error={save.error} />
    </div>
  )
}

const FILTERS = [
  ['all', 'All'],
  ['review', 'Needs review'],
  ['strikes', 'Strikes'],
  ['warn', 'Warns'],
  ['timeout', 'Time-outs'],
  ['kick', 'Kicks'],
  ['ban', 'Bans'],
  ['failed', "Couldn't act"],
  ['undone', 'Undone'],
] as const

export function OutcomeBadge({ a }: { a: Action }) {
  if (a.reversed_at) return <Badge>{a.outcome === 'review' ? 'dismissed' : 'undone'}</Badge>
  if (a.outcome === 'review') return <Badge tone="warn">review</Badge>
  if (a.error) return <Badge tone="danger">couldn't {a.outcome}</Badge>
  return <Badge tone={a.enforced ? (a.outcome === 'ban' || a.outcome === 'kick' ? 'danger' : 'accent') : undefined}>{a.enforced ? a.outcome : `would ${a.outcome}`}</Badge>
}

/** One audit-log table, used by the log and by a user's history. */
export function ActionTable({ serverId, actions, showUser = true }: { serverId: string; actions: Action[]; showUser?: boolean }) {
  const undo = useAction<string>('POST', (id) => `/servers/${serverId}/actions/${id}/undo`, { body: () => ({}) })
  const confirm = useAction<string>('POST', (id) => `/servers/${serverId}/actions/${id}/confirm`, { body: () => ({}) })
  if (!actions.length) return <p className="muted small">Nothing here.</p>
  return (
    <>
      <FormError error={undo.error ?? confirm.error} />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>When</th>
              {showUser && <th>Who</th>}
              <th>Message</th>
              <th>Jev</th>
              <th>Action</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {actions.map((a) => (
              <tr key={a.id}>
                <td className="small">{dateTime(a.created_at)}<br /><span className="faint">#{a.channel_name ?? a.channel_id}</span></td>
                {showUser && (
                  <td>
                    <Link className="mono small" to={`/servers/${serverId}/users/${a.author_id}`}>@{a.username}</Link>
                  </td>
                )}
                <td className="small excerpt">
                  {a.excerpt}
                  {a.message_deleted && <span className="faint tiny"> (deleted)</span>}
                  {a.error && <div className="error tiny">{a.error}</div>}
                </td>
                <td className="small">
                  {a.verdict.replaceAll('_', ' ')}
                  <br />
                  <span className="faint">{percent(badOf(a))} sure</span>
                </td>
                <td>
                  <OutcomeBadge a={a} />
                  {a.strike_number && <div className="faint tiny">strike {a.strike_number}</div>}
                </td>
                <td>
                  {!a.reversed_at && (
                    <div className="row">
                      {a.outcome === 'review' && (
                        <button className="button small" type="button" onClick={() => confirm.mutate(a.id)}>
                          Take action
                        </button>
                      )}
                      <button className="button small ghost" type="button" onClick={() => undo.mutate(a.id)}>
                        {a.outcome === 'review' ? 'Dismiss' : a.enforced ? 'Undo' : 'Wrong call'}
                      </button>
                    </div>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </>
  )
}

function badOf(a: Action) {
  const p = a.probabilities
  return Math.min(1, (p.spam ?? 0) + (p.scam_or_phishing ?? 0) + (p.self_promo_off_topic ?? 0))
}

function Log({ serverId }: { serverId: string }) {
  const [params, setParams] = useSearchParams()
  const filter = params.get('filter') ?? 'all'
  const [pages, setPages] = useState<Action[][]>([])
  const first = useApi<{ actions: Action[] }>(`/servers/${serverId}/actions?filter=${filter}&limit=50`)
  useEffect(() => setPages([]), [filter])
  const rows = [...(first.data?.actions ?? []), ...pages.flat()]
  const more = async () => {
    const last = rows[rows.length - 1]
    if (!last) return
    const next = await api<{ actions: Action[] }>(`/servers/${serverId}/actions?filter=${filter}&limit=50&before=${encodeURIComponent(last.created_at)}`)
    setPages([...pages, next.actions])
  }
  return (
    <div className="stack">
      <div className="row between">
        <nav className="chips" aria-label="Filter">
          {FILTERS.map(([key, label]) => (
            <button key={key} type="button" className={`chip${filter === key ? ' active' : ''}`} onClick={() => setParams(key === 'all' ? {} : { filter: key })}>
              {label}
            </button>
          ))}
        </nav>
        <a className="button small ghost" href={`/api/servers/${serverId}/actions.csv`}>
          Download CSV
        </a>
      </div>
      <Query query={first}>{() => <ActionTable serverId={serverId} actions={rows} />}</Query>
      {rows.length >= 50 && rows.length % 50 === 0 && (
        <button className="button small ghost" type="button" onClick={more}>
          Older
        </button>
      )}
    </div>
  )
}

export function UserPage() {
  const { id = '', user = '' } = useParams()
  const data = useApi<{ user_id: string; live_strikes: number; messages_seen: number; actions: Action[] }>(`/servers/${id}/users/${user}`)
  return (
    <RequireAccount>
      <Query query={data}>
        {(d) => (
          <div className="stack">
            <Link to={`/servers/${id}/log`} className="small">← Audit log</Link>
            <h1 className="mono">@{d.actions[0]?.username ?? d.user_id}</h1>
            <div className="stats">
              <Stat value={d.live_strikes} label="strikes now" />
              <Stat value={d.actions.length} label="times flagged" />
              <Stat value={d.messages_seen} label="messages seen (90 days)" />
            </div>
            <ActionTable serverId={id} actions={d.actions} showUser={false} />
          </div>
        )}
      </Query>
    </RequireAccount>
  )
}
