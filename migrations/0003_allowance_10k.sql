-- Default allowance 5,000 -> 10,000 judged messages a month.
ALTER TABLE guilds ALTER COLUMN monthly_allowance SET DEFAULT 10000;
UPDATE guilds SET monthly_allowance = 10000 WHERE monthly_allowance = 5000;
