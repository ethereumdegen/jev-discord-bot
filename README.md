# Degen Guard

A Discord spam bot any server can add, at **guard.degenbuilders.com**. Jev (TypeSafe's typed
judgments) reads each message; confirmed spam, scams and off-topic self-promotion get the next
step on the server's ladder (default: warn, kick, ban); everything lands in an audit log that
the server's managers can read and undo. See [PLAN.md](PLAN.md).

## How it runs

One binary, `jevmod`. **To start, one Railway service** runs `jevmod all` (`railway.toml`):
the site, the Discord connection and a worker in one process, at one replica.

Later, when it needs to scale, split it into roles (configs in `deploy/`):

| Role | What | Why separate |
|---|---|---|
| `web` | the site and API | can run many copies; its deploys no longer drop the Discord connection |
| `gateway` | the Discord connection: servers joining/leaving, queues messages, buttons and `/guard` | must be exactly one copy |
| `worker` | pops the queue, asks Jev, applies the ladder, writes the log | add copies when the queue backs up |
| `migrate` | runs before each deploy | |

Neon holds accounts, servers, rules, strikes and the audit log. Redis (Upstash) holds the
queue (a stream with a consumer group), cached server settings, cooldowns, message dedupe and
each server's monthly usage. Variables: [.env.example](.env.example).

## Run it locally

Needs Rust, Node 20+, Postgres on 127.0.0.1:5432, `redis-server` and Python 3.

```bash
npm --prefix frontend install && npm --prefix frontend run build
```

```bash
scripts/dev.sh
```

Open http://localhost:3120 and **Continue with Discord**: a local stand-in signs you in as
`localdev`, who manages "Local Builders" and is the operator. Add the bot, then play messages
into it as if someone typed them in Discord:

```bash
scripts/say.sh 5001 "FREE NITRO claim at https://gift.example"
```

The stand-in's Jev calls "free nitro" a scam, "buy my course" spam, "check out my server"
borderline, and everything else fine. There's no gateway locally (the `local` role is web +
worker).

## Tests

```bash
cargo test
```

Each integration test gets a throwaway Postgres (pgtemp) and a `redis-server` on a free port,
with stand-ins for Jev and Discord.

## Discord setup

In the developer portal for the application:

1. OAuth2 redirect: `https://guard.degenbuilders.com/api/auth/discord/callback`.
2. Bot: turn on the **Message Content** intent. It's privileged: fine up to 100 servers; past
   that Discord has to verify the bot and approve the intent.
3. The bot asks for View Channels, Send Messages, Manage Messages, Read Message History, Kick,
   Ban and Moderate Members when it's added. Its role has to sit above the people it acts on;
   it can never act on the server owner or admins (those show as "couldn't act").

## License

MIT. See [LICENSE](LICENSE).
