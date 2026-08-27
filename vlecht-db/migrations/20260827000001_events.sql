-- Knot-local event log for the /events websocket firehose.
-- Matches the Go knotserver schema exactly, including the millisecond-based
-- `created` default; vlecht overrides it with nanosecond TIDs from
-- eventstream.Insert, same as Go's high-water clock.
CREATE TABLE IF NOT EXISTS events (
    rkey    TEXT    NOT NULL,
    nsid    TEXT    NOT NULL,
    event   TEXT    NOT NULL,  -- json
    created INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
    PRIMARY KEY (rkey, nsid)
);
CREATE INDEX IF NOT EXISTS idx_events_created ON events(created);
