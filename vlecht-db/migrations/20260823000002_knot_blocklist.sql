-- Knot-level blocklist (mirrors knot2's sh.tangled.knot.ban/unban).
-- Banned DIDs are denied push, member reads, and XRPC write operations.
-- The knot admin (VLECHT_ATP_OWNER_DID) cannot be banned.

CREATE TABLE knot_blocklist (
    did      TEXT PRIMARY KEY,
    added_by TEXT,
    created  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
