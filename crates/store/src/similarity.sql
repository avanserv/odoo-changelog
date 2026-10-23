-- similarity: how alike a rename's differing bodies are, from 0.5 to 1, when they counted toward
-- its confidence; NULL otherwise. Events of the extractors that sketch bodies are rebuilt by
-- their version bump.
ALTER TABLE events ADD COLUMN similarity REAL;
