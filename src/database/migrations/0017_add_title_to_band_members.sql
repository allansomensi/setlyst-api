-- A free-text "title" for a band member — e.g. "Guitarrista", "Baixista",
-- "Vocalista" — purely for identification/display next to their name on
-- the band page. Deliberately unrelated to `band_members.role` (which
-- governs permissions): this never grants or restricts anything, it's
-- just a label the member (or an admin) sets for themselves.
ALTER TABLE band_members ADD COLUMN title VARCHAR(50);
