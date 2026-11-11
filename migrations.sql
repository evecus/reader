ALTER TABLE users ADD COLUMN auth_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE source_instances ADD COLUMN is_default INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX IF NOT EXISTS source_single_default ON source_instances(is_default) WHERE is_default = 1;
ALTER TABLE book_files ADD COLUMN parse_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE books ADD COLUMN content_hash_kind TEXT NOT NULL DEFAULT 'file';
ALTER TABLE chapter_resources ADD COLUMN media_type TEXT NOT NULL DEFAULT 'text/plain';
ALTER TABLE user_books ADD COLUMN reading_status TEXT;
