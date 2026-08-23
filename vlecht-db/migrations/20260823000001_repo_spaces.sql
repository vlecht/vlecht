-- Private repo access control (knot-hosted atproto spaces).
--
-- Each private repo maps to a space at
-- at://<knot-did>/space/sh.tangled.repo/<repo-did>, with this DB as the
-- membership source. A repo with no row in repo_visibility is public.
-- Members may clone/read; pushes remain owner-only.

CREATE TABLE repo_visibility (
    repo_did   TEXT PRIMARY KEY,
    visibility TEXT NOT NULL CHECK (visibility IN ('public', 'private'))
);

CREATE TABLE repo_members (
    repo_did   TEXT NOT NULL,
    member_did TEXT NOT NULL,
    -- `reader` may clone/fetch; `writer` may additionally push (= Go
    -- knotserver's "collaborator").
    role       TEXT NOT NULL DEFAULT 'reader' CHECK (role IN ('reader', 'writer')),
    added_by   TEXT,
    created    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    PRIMARY KEY (repo_did, member_did)
);
CREATE INDEX idx_repo_members_member ON repo_members(member_did);
