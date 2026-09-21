# jev-discord-bot — plan

2026-09-18. Status: **J0–J5 built and tested locally (Degen Guard, guard.degenbuilders.com);
J6 billing and J7 custom rules not built; nothing deployed (J8).** See "What was built" at the end.

A spam-moderation bot anyone can add to their Discord. Jev (TypeSafe's typed judgments) reads
every message and decides whether it breaks the server's rules; confirmed offenders are warned,
kicked or banned on a ladder the server owner sets; everything lands in an audit log.

## 1. How an owner uses it

1. **Sign in with your Degen Builders account** on the bot's website.
2. **Add to Discord.** One Discord OAuth screen does two things: installs the bot in the
   server they pick (scopes `bot applications.commands`, permissions Manage Messages, Kick,
   Ban, Moderate Members, View Channels, Read Message History) and tells us who they are on
   Discord (`identify guilds`). We only attach a server to their account if Discord says they
   can Manage Server in it.
3. **Their server gets one rule, on:** *No spam* (spam, scams and phishing, off-topic
   self-promotion). They can choose:
   - **Mode:** *watch* (log what it would do, touch nothing) or *enforce*. New servers start in
     watch for a few days so the owner can see what it catches first.
   - **The ladder** for confirmed offenses. Default: 1st **warn**, 2nd **kick**, 3rd **ban**.
     Each step can be none / warn / time-out / kick / ban. Strikes expire after 30 days
     (changeable).
   - **How sure Jev must be** for an offense to count (default 90%). Messages between the
     flag level (60%) and that are listed for review, with no strike.
   - **Exempt roles and channels:** mods, trusted members, a #self-promo channel.
   - **A log channel** (optional): each action posted in Discord with Undo / Wrong-call buttons.
4. **The audit log** on the website shows every message the bot acted on.

Later (not the first version): **custom rules** in plain English ("no crypto shilling", "no
job posts outside #jobs"). Each becomes one more Jev yes/no question about the same message.

## 2. What happens to one message

```
Discord ──gateway──▶ bot service ──▶ skip? (bots, exempt roles/channels, the owner, trusted
                         │                   regulars' plain chat, quota used up)
                         ▼
                 Jev: which kind is this? how sure? does it lure people to click/DM/pay?
                         │
          below flag ◀───┼───▶ between flag and "sure": listed for review, no strike
                         ▼
               confirmed offense: strike N for this user in this server
                         ▼
          ladder step N: warn (delete + a notice) / time-out / kick / ban
                         ▼
          audit log row (Neon) + log channel post + the server's counters
```

- **A warn** deletes the message and posts a short notice in the channel mentioning the user
  (DMs are often blocked), which deletes itself after a minute.
- **The bot can't act on** the server owner, administrators, or anyone whose top role is above
  the bot's. Those are logged as "couldn't act" instead of failing silently.
- **Undo** (website or log-channel button, mods only): unbans or lifts the time-out, removes
  the strike, and marks the verdict wrong so the thresholds can be tuned.

## 3. The audit log

Per server, on the website:

| Column | |
|---|---|
| When, channel | |
| User | name, account age, their strike count; click through to their history |
| Message | the excerpt (first 500 characters) and whether it was deleted |
| Verdict | spam / scam / self-promo, how sure, the lure score |
| Action | warn / time-out / kick / ban / review / "would have" (watch mode) / couldn't act |
| Undone? | by whom |

Filters: action, user, rule, date; a "needs review" tab; a user page (every infraction for one
person); export to CSV. Totals on top: messages judged, offenses, bans this week, Jev usage.

**Privacy:** only messages the bot acted on or flagged are stored, never ordinary chat. Rows
older than 90 days are deleted. Removing the bot from a server deletes that server's data after
a 7-day grace period.

## 4. Storage: Neon and Upstash

- **Neon Postgres is the source of truth**: accounts, servers, owners, rules and ladders,
  strikes, and the audit log. The log needs filtering, joins and months of history, which is
  what Postgres is for; Redis is the wrong home for it.
- **Upstash Redis is the hot path**, so a busy server doesn't hit Postgres per message:
  - each server's config, cached (the website clears it on save);
  - per-user cooldowns (a flood is judged once, not 50 times) and message-id dedupe;
  - usage counters for each server's monthly quota;
  - a queue (Redis stream) between the gateway, which must never stall, and the workers that
    call Jev, so a spam raid is absorbed instead of dropped.

## 5. Services

```
jev-bot-web      Axum + React: Degen Builders sign-in, "Add to Discord", server settings, audit log
jev-bot-gateway  holds the Discord gateway for every server (sharded as it grows); pushes messages to Redis
jev-bot-worker   pops messages, calls Jev, applies the ladder, writes Neon, calls Discord REST
Neon · Upstash · Jev (TypeSafe) · Discord
```

The engine already written (`rules.rs`, `judge.rs`, `bot.rs`) becomes the worker. Its settings
move from environment variables to per-server rows.

## 6. Costs and Discord's rules

- **Every judged message is a Jev call** on our key. The pre-filters (exempt roles, trusted
  regulars, cooldowns) cut most of them, but a busy server can still cost real money, so each
  server gets a monthly quota: a free tier, and a paid plan above it. When the quota runs out,
  the bot keeps logging and stops judging.
- **Message Content is a privileged intent.** Up to 100 servers it just needs switching on;
  past 100 the bot must be verified by Discord and the intent approved (moderation is an
  accepted reason). Plan for that before launch marketing.
- **Sharding** becomes mandatory at 2,500 servers; start with one shard and move to
  Discord's recommended count later.

## 7. Phases

| | |
|---|---|
| **J0** | done: the engine for one server |
| **J1** | done: multi-tenant schema; the engine reads per-server settings |
| **J2** | done: the ladder, "couldn't act", strike expiry |
| **J3** | done: website with Degen Builders or Discord sign-in, Add to Discord with an ownership check, settings |
| **J4** | done: audit log with filters, paging, review tab, user history, undo and take-action, CSV |
| **J5** | done: Redis stream queue, settings cache, cooldowns, dedupe, monthly allowance (free tier, 2,000/server, operator can raise) |
| **J6** | billing: **not needed yet** (the owner pays for Jev; allowances are set by the operator) |
| **J7** | custom plain-English rules: not built |
| **J8** | deploy: not started (see What was built) |

Degen Builders' server becomes the first tenant.

## What was built (2026-09-18)

- **Name and home:** Degen Guard at guard.degenbuilders.com, its own Railway project: one
  service (`jevmod all`) to start; web / gateway / worker split later if needed.
- **Defaults chosen:** 2,000 judged messages per server per month (the operator raises it);
  past it the bot keeps logging nothing new and tells the log channel once; warnings are a
  channel notice that deletes itself after a minute; new servers start in watch mode.
- **Sign-in:** Degen Builders or Discord. This site keeps no sign-in of its own: it sends the
  browser to degenbuilders.com (`/api/v1/sso/authorize`) and swaps the code it carries back for
  the identity over a back channel (`SSO_CLIENT_ID` / `SSO_CLIENT_SECRET`). Land here already
  signed in there and you're signed in here, silently, once per tab. Only Discord can prove
  which servers you manage, so a Builders account connects Discord before it sees servers.
- **Every judged message is one Jev call** (a kind choice and a lure yes/no, with the server's
  own description of what's normal there).
- **Tests:** 4 unit + 7 integration (pgtemp + a real `redis-server`, stand-ins for Jev and
  Discord). Checked in the browser against the local stand-ins: sign-in, Add to Discord,
  settings, enforce, messages through the real queue and worker, the audit log, take action,
  375px layout.
- **Found by running it, not by the tests:** the worker's blocking queue read shared the Redis
  connection with everything else and timed out (and stalled other calls behind it). Workers now
  read on their own connection; the test blocks longer than the old timeout.
- **Never touched real:** Discord (OAuth, the gateway, slash commands, buttons, kicks and bans),
  the real Jev API and how good its verdicts are, the Degen Builders hand-off, Upstash, Neon, the Docker build.

**To go live (J8):** a Discord application and bot (Message Content intent on), a new Railway
project with the three services, a Neon database, an Upstash Redis, the Jev key, and DNS for
guard.degenbuilders.com. Degen Builders' own server is the first to add it.
