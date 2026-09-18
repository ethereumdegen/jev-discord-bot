-- Jev Mod: accounts (people who sign in), the Discord servers they manage,
-- each server's rules and punishment ladder, strikes, and the audit log.

CREATE TABLE accounts (
    id uuid PRIMARY KEY,
    email text,
    name text NOT NULL,
    avatar_url text,
    -- Their Discord identity, once they've connected it.
    discord_user_id text UNIQUE,
    discord_username text,
    -- Runs the bot: sees every server, sets allowances.
    is_operator boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX accounts_email_idx ON accounts (lower(email)) WHERE email IS NOT NULL;

CREATE TABLE account_identities (
    provider text NOT NULL CHECK (provider IN ('google', 'discord', 'email')),
    subject text NOT NULL,
    account_id uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, subject)
);

CREATE TABLE sessions (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE,
    csrf_hash bytea NOT NULL,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE oauth_attempts (
    state_hash bytea PRIMARY KEY,
    provider text NOT NULL,
    -- sign_in (Google), connect (Discord identity + servers) or install (add the bot).
    purpose text NOT NULL,
    account_id uuid REFERENCES accounts(id) ON DELETE CASCADE,
    browser_hash bytea NOT NULL,
    nonce text NOT NULL,
    pkce_verifier text NOT NULL,
    return_path text NOT NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz
);

CREATE TABLE magic_links (
    token_hash bytea PRIMARY KEY,
    email text NOT NULL,
    return_path text NOT NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz
);

-- The servers an account can manage, as Discord last told us (Manage Server or owner).
CREATE TABLE account_guilds (
    account_id uuid NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    guild_id text NOT NULL,
    name text NOT NULL,
    icon text,
    owner boolean NOT NULL DEFAULT false,
    refreshed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, guild_id)
);

-- A server the bot is in (or was).
CREATE TABLE guilds (
    id text PRIMARY KEY,
    name text NOT NULL,
    icon text,
    owner_user_id text,
    installed_at timestamptz NOT NULL DEFAULT now(),
    removed_at timestamptz,
    -- watch: log what it would do. enforce: act. paused: ignore messages.
    mode text NOT NULL DEFAULT 'watch' CHECK (mode IN ('watch', 'enforce', 'paused')),
    log_channel_id text,
    exempt_role_ids text[] NOT NULL DEFAULT '{}',
    exempt_channel_ids text[] NOT NULL DEFAULT '{}',
    confident_percent integer NOT NULL DEFAULT 90 CHECK (confident_percent BETWEEN 1 AND 100),
    flag_percent integer NOT NULL DEFAULT 60 CHECK (flag_percent BETWEEN 1 AND 100),
    strike_days integer NOT NULL DEFAULT 30 CHECK (strike_days BETWEEN 1 AND 3650),
    timeout_minutes integer NOT NULL DEFAULT 60 CHECK (timeout_minutes BETWEEN 1 AND 40320),
    -- Given to Jev with every message, so it knows what's normal here.
    community text NOT NULL DEFAULT '',
    -- Judged messages per calendar month (UTC); past it the bot logs nothing new.
    monthly_allowance integer NOT NULL DEFAULT 2000 CHECK (monthly_allowance >= 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (flag_percent <= confident_percent)
);

CREATE TABLE rules (
    id uuid PRIMARY KEY,
    guild_id text NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    -- spam: the built-in rule. custom: a plain-English rule (later).
    kind text NOT NULL CHECK (kind IN ('spam', 'custom')),
    name text NOT NULL,
    instruction text NOT NULL DEFAULT '',
    enabled boolean NOT NULL DEFAULT true,
    -- What each strike does, in order: none, warn, timeout, kick or ban. Past the
    -- end, the last step repeats.
    ladder text[] NOT NULL DEFAULT '{warn,kick,ban}',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX rules_one_spam ON rules (guild_id) WHERE kind = 'spam';

-- Every message the bot flagged or acted on: the audit log.
CREATE TABLE actions (
    id uuid PRIMARY KEY,
    guild_id text NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    rule_id uuid REFERENCES rules(id) ON DELETE SET NULL,
    author_id text NOT NULL,
    username text NOT NULL,
    channel_id text NOT NULL,
    channel_name text,
    message_id text NOT NULL UNIQUE,
    excerpt text NOT NULL,
    verdict text NOT NULL,
    probabilities jsonb NOT NULL,
    confidence double precision NOT NULL,
    lure double precision NOT NULL,
    -- review: flagged, below the confident line, no strike. The rest are ladder steps.
    outcome text NOT NULL CHECK (outcome IN ('review', 'none', 'warn', 'timeout', 'kick', 'ban')),
    -- Which strike this was (NULL for review).
    strike_number integer,
    -- False in watch mode, or when Discord refused (see error).
    enforced boolean NOT NULL,
    message_deleted boolean NOT NULL DEFAULT false,
    error text,
    model text,
    input_tokens integer,
    output_tokens integer,
    created_at timestamptz NOT NULL DEFAULT now(),
    reversed_at timestamptz,
    reversed_by text,
    marked_wrong boolean NOT NULL DEFAULT false
);
CREATE INDEX actions_guild_idx ON actions (guild_id, created_at DESC);
CREATE INDEX actions_author_idx ON actions (guild_id, author_id, created_at DESC);

CREATE TABLE strikes (
    id uuid PRIMARY KEY,
    guild_id text NOT NULL REFERENCES guilds(id) ON DELETE CASCADE,
    user_id text NOT NULL,
    action_id uuid NOT NULL REFERENCES actions(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    cleared_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX strikes_live_idx ON strikes (guild_id, user_id) WHERE cleared_at IS NULL;
