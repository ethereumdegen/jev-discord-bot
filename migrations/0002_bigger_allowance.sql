-- Every message is judged now (no skipping regulars or floods), so the
-- default allowance goes from 2,000 to 5,000 judged messages a month.
ALTER TABLE guilds ALTER COLUMN monthly_allowance SET DEFAULT 5000;
UPDATE guilds SET monthly_allowance = 5000 WHERE monthly_allowance = 2000;
