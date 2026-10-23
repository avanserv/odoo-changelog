-- Events are rebuilt from the blob cache: a rename now lists the symbols that name it.
-- referrers: a rename's JSON array of the qualnames whose change in the same commit replaced the
-- old name with the new one, the extractor's own symbols only; NULL for every other op.
DELETE FROM events;
DELETE FROM extracted;
DELETE FROM symbols;
UPDATE commits SET category = NULL, breaking = NULL, rule_id = NULL, classified = NULL;
ALTER TABLE events ADD COLUMN referrers TEXT;
