-- What a change means to its readers, as docs/classify.md lists the rules. category: functional
-- or technical; breaking: 1 when custom code written against the old side may fail; rule_id:
-- the rule that decided them. NULL until extract classifies the commit.
ALTER TABLE events ADD COLUMN category TEXT;
ALTER TABLE events ADD COLUMN breaking INTEGER;
ALTER TABLE events ADD COLUMN rule_id TEXT;

-- The same for a commit, from its events and tag; category stays NULL for a commit without
-- events. classified: the classifier version that wrote them, NULL once its events change.
ALTER TABLE commits ADD COLUMN category TEXT;
ALTER TABLE commits ADD COLUMN breaking INTEGER;
ALTER TABLE commits ADD COLUMN rule_id TEXT;
ALTER TABLE commits ADD COLUMN classified INTEGER;
