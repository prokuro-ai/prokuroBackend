CREATE TABLE IF NOT EXISTS part_cache (
  mpn TEXT NOT NULL,
  manufacturer TEXT NOT NULL DEFAULT '',
  result JSONB NOT NULL,
  fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (mpn, manufacturer)
);
