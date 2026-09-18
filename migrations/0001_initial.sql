-- Every message the bot judged and acted on (or, in shadow mode, would have),
-- what Jev said, and whether a mod undid it.
CREATE TABLE actions (
    id uuid PRIMARY KEY,
    guild_id text NOT NULL,
    author_id text NOT NULL,
    username text NOT NULL,
    channel_id text NOT NULL,
    message_id text NOT NULL UNIQUE,
    excerpt text NOT NULL,
    -- new_visitor, visitor or member.
    sender text NOT NULL CHECK (sender IN ('new_visitor', 'visitor', 'member')),
    verdict text NOT NULL,
    probabilities jsonb NOT NULL,
    confidence double precision NOT NULL,
    lure double precision NOT NULL,
    action text NOT NULL CHECK (action IN ('flag', 'delete', 'timeout', 'kick', 'ban')),
    enforced boolean NOT NULL,
    model text,
    input_tokens integer,
    output_tokens integer,
    created_at timestamptz NOT NULL DEFAULT now(),
    reversed_at timestamptz,
    reversed_by text,
    marked_wrong boolean NOT NULL DEFAULT false
);
CREATE INDEX actions_created_idx ON actions (created_at DESC);

-- What the bot remembers about each author.
CREATE TABLE authors (
    guild_id text NOT NULL,
    author_id text NOT NULL,
    messages integer NOT NULL DEFAULT 0,
    strikes integer NOT NULL DEFAULT 0,
    last_judged_at timestamptz,
    PRIMARY KEY (guild_id, author_id)
);

-- Runtime settings mods change with /jev (the mode); env gives the defaults.
CREATE TABLE settings (
    guild_id text NOT NULL,
    key text NOT NULL,
    value text NOT NULL,
    updated_by text,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, key)
);
