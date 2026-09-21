import { Link, useSearchParams } from 'react-router-dom'
import { SignIn } from '../components/ui'
import { useMe } from '../hooks/api'

const ERRORS: Record<string, string> = {
  discord: "Discord sign-in didn't finish. Try again.",
  sso: "Degen Builders sign-in didn't finish. Try again.",
}

export function HomePage() {
  const { account } = useMe()
  const [params] = useSearchParams()
  const error = params.get('error')
  return (
    <div className="stack-lg">
      <section className="hero stack">
        <p className="eyebrow">a spam bot for discord, judged by jev</p>
        <h1>Keep spam out of your Discord.</h1>
        <p className="lede">
          Degen Guard reads every message in your server. When Jev is sure it's spam, a scam or off-topic self-promotion, the bot warns, kicks or bans on the ladder you choose, and
          every call lands in an audit log you can undo from.
        </p>
        {error && <p className="notice danger small">{ERRORS[error] ?? 'Sign-in failed.'}</p>}
        {account ? (
          <Link className="button primary big" to="/servers">
            Your servers
          </Link>
        ) : (
          <SignIn />
        )}
      </section>
      <section className="perks">
        <div className="perk">
          <strong>1. Add it</strong>
          <span className="muted small">Sign in, pick a server you manage, and add the bot. It starts in watch mode: it logs what it would do and touches nothing.</span>
        </div>
        <div className="perk">
          <strong>2. Set the ladder</strong>
          <span className="muted small">By default a first offense is a warning, the second a kick, the third a ban. Strikes wear off after 30 days. Change any of it.</span>
        </div>
        <div className="perk">
          <strong>3. Read the log</strong>
          <span className="muted small">Every flagged message, who sent it, what Jev said and what the bot did. Undo a call with one click, from the site or from Discord.</span>
        </div>
      </section>
      <section className="card stack">
        <h3 className="prompt">what it won't do</h3>
        <ul className="stack small muted">
          <li>Read ordinary chat from your regulars: people who've been around a month and talked a while skip the check unless they post links.</li>
          <li>Touch the server owner, admins, roles you exempt, or channels you exempt (like #self-promo).</li>
          <li>Keep your messages: it stores only the ones it flagged, and deletes them after 90 days.</li>
        </ul>
      </section>
    </div>
  )
}
