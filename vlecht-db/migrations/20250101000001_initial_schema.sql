-- Base tables matching the Go knotserver schema exactly.
-- This allows drop-in migration: stop the Go server, start the Rust server on the same DB.

CREATE TABLE known_dids (
    did TEXT PRIMARY KEY
);

CREATE TABLE public_keys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    did TEXT NOT NULL,
    key TEXT NOT NULL,
    created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(did, key),
    FOREIGN KEY (did) REFERENCES known_dids(did) ON DELETE CASCADE
);

CREATE TABLE _jetstream (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    last_time_us INTEGER NOT NULL
);

CREATE TABLE events (
    rkey TEXT NOT NULL,
    nsid TEXT NOT NULL,
    event TEXT NOT NULL,
    created INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    PRIMARY KEY (rkey, nsid)
);

CREATE TABLE repo_keys (
    repo_did    TEXT PRIMARY KEY,
    signing_key BLOB,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    owner_did   TEXT,
    repo_name   TEXT,
    key_type    TEXT NOT NULL DEFAULT 'k256'
);
CREATE UNIQUE INDEX idx_repo_keys_owner_repo ON repo_keys(owner_did, repo_name);

CREATE TABLE migrations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT UNIQUE
);

CREATE TABLE repo_aliases (
    owner_did TEXT NOT NULL,
    rkey      TEXT NOT NULL,
    repo_did  TEXT NOT NULL,
    rev       TEXT NOT NULL,
    PRIMARY KEY (owner_did, rkey)
);
CREATE INDEX idx_repo_aliases_repo_did ON repo_aliases(repo_did);

CREATE TABLE knot_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    did TEXT NOT NULL,
    rkey TEXT NOT NULL,
    subject TEXT NOT NULL,
    created TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (did, rkey)
);
