PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS books (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    authors_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(authors_json) AND json_type(authors_json) = 'array'),
    author_sort TEXT NOT NULL DEFAULT '',
    series TEXT,
    series_index REAL CHECK (series_index IS NULL OR series_index >= 0),
    genres_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(genres_json) AND json_type(genres_json) = 'array'),
    tags_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(tags_json) AND json_type(tags_json) = 'array'),
    language TEXT NOT NULL DEFAULT 'und',
    description TEXT NOT NULL DEFAULT '',
    isbn TEXT,
    publisher TEXT,
    published TEXT,
    cover_relative_path TEXT,
    read_status TEXT NOT NULL DEFAULT 'unread' CHECK (length(trim(read_status)) > 0),
    reading_progress REAL NOT NULL DEFAULT 0 CHECK (reading_progress BETWEEN 0 AND 1),
    reader_location TEXT,
    favorite INTEGER NOT NULL DEFAULT 0 CHECK (favorite IN (0, 1)),
    rating REAL CHECK (rating IS NULL OR rating BETWEEN 0 AND 5),
    notes TEXT NOT NULL DEFAULT '',
    metadata_status TEXT NOT NULL DEFAULT 'pending' CHECK (length(trim(metadata_status)) > 0),
    metadata_confidence REAL CHECK (metadata_confidence IS NULL OR metadata_confidence BETWEEN 0 AND 1),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    added_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE IF NOT EXISTS book_files (
    id TEXT PRIMARY KEY NOT NULL,
    book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format TEXT NOT NULL CHECK (length(trim(format)) > 0),
    variant TEXT NOT NULL DEFAULT 'original' CHECK (length(trim(variant)) > 0),
    profile TEXT,
    relative_path TEXT NOT NULL UNIQUE CHECK (length(relative_path) > 0),
    sha256 TEXT NOT NULL UNIQUE CHECK (length(sha256) = 64),
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE IF NOT EXISTS devices (
    id TEXT PRIMARY KEY NOT NULL,
    label TEXT NOT NULL CHECK (length(trim(label)) > 0),
    transport TEXT NOT NULL CHECK (length(trim(transport)) > 0),
    profile TEXT NOT NULL DEFAULT 'generic',
    mount_identity TEXT,
    last_seen_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE IF NOT EXISTS device_books (
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL CHECK (length(relative_path) > 0),
    book_id TEXT REFERENCES books(id) ON DELETE SET NULL,
    sha256 TEXT CHECK (sha256 IS NULL OR length(sha256) = 64),
    title TEXT NOT NULL DEFAULT '',
    authors_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(authors_json) AND json_type(authors_json) = 'array'),
    format TEXT NOT NULL CHECK (length(trim(format)) > 0),
    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
    last_seen_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (device_id, relative_path)
) STRICT;

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (length(trim(kind)) > 0),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (length(trim(status)) > 0),
    progress REAL NOT NULL DEFAULT 0 CHECK (progress BETWEEN 0 AND 1),
    payload_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload_json)),
    result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)),
    error_json TEXT CHECK (error_json IS NULL OR json_valid(error_json)),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE IF NOT EXISTS operations (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (length(trim(kind)) > 0),
    status TEXT NOT NULL CHECK (length(trim(status)) > 0),
    before_json TEXT CHECK (before_json IS NULL OR json_valid(before_json)),
    after_json TEXT CHECK (after_json IS NULL OR json_valid(after_json)),
    backup_relative_path TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY NOT NULL CHECK (length(key) > 0),
    value_json TEXT NOT NULL CHECK (json_valid(value_json))
) STRICT;

CREATE TABLE IF NOT EXISTS provider_catalogs (
    provider_id TEXT PRIMARY KEY NOT NULL,
    models_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(models_json) AND json_type(models_json) = 'array'),
    source TEXT NOT NULL,
    fetched_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_error TEXT
) STRICT;

CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('system', 'user', 'assistant', 'tool')),
    content TEXT NOT NULL,
    sources_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(sources_json) AND json_type(sources_json) = 'array'),
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE VIRTUAL TABLE IF NOT EXISTS books_search USING fts5(
    book_id UNINDEXED,
    title,
    authors,
    series,
    genres,
    description,
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE INDEX IF NOT EXISTS idx_books_author_sort ON books(author_sort COLLATE NOCASE, title COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_books_series ON books(series COLLATE NOCASE, series_index);
CREATE INDEX IF NOT EXISTS idx_books_language ON books(language);
CREATE INDEX IF NOT EXISTS idx_books_read_status ON books(read_status);
CREATE INDEX IF NOT EXISTS idx_books_metadata_status ON books(metadata_status);
CREATE INDEX IF NOT EXISTS idx_book_files_book ON book_files(book_id);
CREATE INDEX IF NOT EXISTS idx_device_books_book ON device_books(book_id, device_id);
CREATE INDEX IF NOT EXISTS idx_device_books_sha256 ON device_books(sha256);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status, created_at);
CREATE INDEX IF NOT EXISTS idx_messages_conversation ON messages(conversation_id, created_at);
