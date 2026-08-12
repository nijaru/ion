package session

import (
	"errors"
	"time"
)

// SQLite schema constants and migration errors are shared by the single SQLiteStore owner.
const currentSchemaVersion = 11

const sessionLockWait = 500 * time.Millisecond

var (
	ErrSessionClosed     = errors.New("session store is closed")
	ErrSessionBusy       = errors.New("session store is busy")
	ErrUnsupportedSchema = errors.New("unsupported session schema")
	ErrCorruptSession    = errors.New("corrupt session store")
	ErrTurnNotFound      = errors.New("turn not found")
	ErrTurnState         = errors.New("invalid turn state")
	ErrTurnEntryConflict = errors.New("turn entry conflict")
)

const sessionCatalogLookupSQL = `WITH RECURSIVE
	input_ancestors(id) AS (
		SELECT ?
		UNION
		SELECT e.parent_id
		FROM entries e
		JOIN input_ancestors ON e.id = input_ancestors.id
		WHERE input_ancestors.id <> '' AND e.parent_id <> ''
	),
	session_ancestors(session_id, id) AS (
		SELECT s.session_id, s.leaf_id
		FROM sessions s
		UNION
		SELECT session_ancestors.session_id, e.parent_id
		FROM entries e
		JOIN session_ancestors ON e.id = session_ancestors.id
		WHERE session_ancestors.id <> '' AND e.parent_id <> ''
	),
	matches(session_id) AS (
		SELECT DISTINCT session_ancestors.session_id
		FROM input_ancestors
		JOIN session_ancestors ON session_ancestors.id = input_ancestors.id
	)
	SELECT sessions.session_id, sessions.leaf_id, sessions.workdir, sessions.model,
		sessions.branch, sessions.name, sessions.summary, sessions.last_preview, sessions.updated_at
	FROM sessions
	LEFT JOIN matches ON matches.session_id = sessions.session_id
	WHERE sessions.session_id = ? OR matches.session_id IS NOT NULL
	ORDER BY
		CASE WHEN sessions.session_id = ? THEN 0
		     WHEN sessions.leaf_id = ? THEN 1
		     ELSE 2 END,
		sessions.updated_at DESC
	LIMIT 1`

// Schema holds the SQL for creating the tables.
const Schema = `
CREATE TABLE IF NOT EXISTS entries (
	id         TEXT PRIMARY KEY,
	parent_id  TEXT NOT NULL DEFAULT '',
	type       TEXT NOT NULL,
	timestamp  INTEGER NOT NULL,
	sequence   INTEGER NOT NULL DEFAULT 0,
	turn_id    TEXT,
	payload    BLOB NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_entries_parent ON entries(parent_id);
CREATE INDEX IF NOT EXISTS idx_entries_type ON entries(type);
CREATE INDEX IF NOT EXISTS idx_entries_sequence ON entries(sequence);
CREATE INDEX IF NOT EXISTS idx_entries_turn ON entries(turn_id);

CREATE TABLE IF NOT EXISTS session_meta (
	key   TEXT PRIMARY KEY,
	value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS turns (
	turn_id    TEXT PRIMARY KEY,
	sequence   INTEGER NOT NULL,
	state      TEXT NOT NULL,
	leaf_id    TEXT NOT NULL DEFAULT '',
	input      TEXT NOT NULL DEFAULT '',
	input_images BLOB NOT NULL DEFAULT '[]',
	context_id TEXT NOT NULL DEFAULT '',
	started_at INTEGER NOT NULL,
	ended_at   INTEGER NOT NULL DEFAULT 0,
	error      TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_turns_state ON turns(state, sequence);

CREATE TABLE IF NOT EXISTS actions (
	action_id        TEXT PRIMARY KEY,
	invocation_id    TEXT NOT NULL,
	session_id       TEXT NOT NULL DEFAULT '',
	turn_id          TEXT NOT NULL DEFAULT '',
	tool_name        TEXT NOT NULL,
	category         TEXT NOT NULL DEFAULT '',
	operation        TEXT NOT NULL,
	arguments        BLOB NOT NULL,
	metadata         BLOB NOT NULL DEFAULT '{}',
	preimages        BLOB NOT NULL DEFAULT '[]',
	fingerprint      TEXT NOT NULL,
	cwd              TEXT NOT NULL DEFAULT '',
	paths            BLOB NOT NULL DEFAULT '[]',
	environment      BLOB NOT NULL DEFAULT '[]',
	network_intent   TEXT NOT NULL DEFAULT '',
	mcp_identity     TEXT NOT NULL DEFAULT '',
	policy_mode      TEXT NOT NULL DEFAULT '',
	state            TEXT NOT NULL,
	authorization    TEXT NOT NULL DEFAULT '',
	result_identity  TEXT NOT NULL DEFAULT '',
	error            TEXT NOT NULL DEFAULT '',
	cleanup_outcome  TEXT NOT NULL DEFAULT '',
	process_identity TEXT NOT NULL DEFAULT '',
	prepared_at      INTEGER NOT NULL,
	authorized_at    INTEGER NOT NULL DEFAULT 0,
	started_at       INTEGER NOT NULL DEFAULT 0,
	ended_at         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_actions_state ON actions(state, prepared_at);
CREATE INDEX IF NOT EXISTS idx_actions_fingerprint ON actions(fingerprint);

CREATE TABLE IF NOT EXISTS action_transitions (
	id         INTEGER PRIMARY KEY AUTOINCREMENT,
	action_id  TEXT NOT NULL,
	from_state TEXT NOT NULL DEFAULT '',
	to_state   TEXT NOT NULL,
	reason     TEXT NOT NULL DEFAULT '',
	timestamp  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_action_transitions_action ON action_transitions(action_id, id);

CREATE TABLE IF NOT EXISTS input_history (
	id        INTEGER PRIMARY KEY AUTOINCREMENT,
	workdir   TEXT NOT NULL,
	input     TEXT NOT NULL,
	timestamp INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_input_workdir ON input_history(workdir, timestamp);

CREATE TABLE IF NOT EXISTS sessions (
	session_id   TEXT PRIMARY KEY,
	leaf_id      TEXT NOT NULL DEFAULT '',
	workdir      TEXT NOT NULL,
	model        TEXT NOT NULL DEFAULT '',
	branch       TEXT NOT NULL DEFAULT '',
	name         TEXT NOT NULL DEFAULT '',
	summary      TEXT NOT NULL DEFAULT '',
	last_preview TEXT NOT NULL DEFAULT '',
	updated_at   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_sessions_workdir ON sessions(workdir, updated_at);
CREATE INDEX IF NOT EXISTS idx_sessions_leaf ON sessions(leaf_id);
`
