-- Signing in is degenbuilders.com's job now: an identity can come from there.
-- 'google' stays allowed so identities from the old sign-in still resolve.
ALTER TABLE account_identities DROP CONSTRAINT account_identities_provider_check;
ALTER TABLE account_identities ADD CONSTRAINT account_identities_provider_check
    CHECK (provider IN ('builders', 'google', 'discord', 'email'));
