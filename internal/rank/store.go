// Package rank implements adaptive ranking for argmax: workspace-signature
// detection, the private local SQLite learning store (frecency and
// command-skeleton transitions), and the deterministic weighted scoring
// engine (PRD 9.11).
package rank

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

// queryTimeout bounds every individual store query so learning lookups can
// never stall the completion path (RANK-012).
const queryTimeout = 500 * time.Millisecond

// openTimeout bounds schema creation during Open, which may pay for WAL
// recovery on a cold database file.
const openTimeout = 5 * time.Second

// pruneAge is how long learned rows are kept (PRD 9.11: forget rows older
// than 90 days).
const pruneAge = 90 * 24 * time.Hour

// globalFallbackScale is the strength of the global (all-CWD) aggregate when
// no exact current-CWD row exists (RANK-004, RANK-005).
const globalFallbackScale = 0.7

const schema = `
CREATE TABLE IF NOT EXISTS usage (
	cwd       TEXT NOT NULL,
	skeleton  TEXT NOT NULL,
	success   INTEGER NOT NULL DEFAULT 0,
	last_used INTEGER NOT NULL,
	PRIMARY KEY (cwd, skeleton)
);
CREATE TABLE IF NOT EXISTS transitions (
	cwd       TEXT NOT NULL,
	prev      TEXT NOT NULL,
	cur       TEXT NOT NULL,
	count     INTEGER NOT NULL DEFAULT 0,
	last_used INTEGER NOT NULL,
	PRIMARY KEY (cwd, prev, cur)
);
CREATE INDEX IF NOT EXISTS idx_usage_skeleton ON usage (skeleton);
CREATE INDEX IF NOT EXISTS idx_transitions_pair ON transitions (prev, cur);
`

// Store is the private local SQLite learning database (RANK-001). It is safe
// for concurrent use. All methods tolerate a nil receiver or a nil underlying
// handle: storage errors degrade callers to static ranking, so query methods
// return zero values on error instead of failing (RANK-012).
type Store struct {
	db *sql.DB
}

// Open creates or opens the learning database at path. The database runs in
// WAL mode with a single process-local connection and a five-second busy
// timeout (PRD 9.15); the file is created with mode 0600 and a missing parent
// directory with mode 0700. A corrupted or incompatible database file is
// forgotten and created fresh. An error is returned only when no usable
// database could be produced at all; callers should then rank statically.
func Open(path string) (*Store, error) {
	dir := filepath.Dir(path)
	if _, err := os.Stat(dir); err != nil {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			return nil, fmt.Errorf("rank: create state dir: %w", err)
		}
		// MkdirAll honors the umask; the state directory must be private.
		_ = os.Chmod(dir, 0o700)
	}
	// Create the file up front so its mode is 0600 regardless of umask, and
	// tighten the mode of a pre-existing database file (PRD 9.15).
	if f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600); err != nil {
		return nil, fmt.Errorf("rank: create database file: %w", err)
	} else {
		_ = f.Close()
	}
	_ = os.Chmod(path, 0o600)

	db, err := openDB(path)
	if err != nil {
		// Corrupted file or incompatible schema: forget and start fresh.
		removeDBFiles(path)
		db, err = openDB(path)
		if err != nil {
			return nil, fmt.Errorf("rank: open database: %w", err)
		}
	}
	return &Store{db: db}, nil
}

func openDB(path string) (*sql.DB, error) {
	db, err := sql.Open("sqlite", path+"?_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)")
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	if err := initSchema(db); err != nil {
		_ = db.Close()
		return nil, err
	}
	return db, nil
}

func removeDBFiles(path string) {
	_ = os.Remove(path)
	_ = os.Remove(path + "-wal")
	_ = os.Remove(path + "-shm")
}

// initSchema creates the schema when missing and probes the expected columns
// so an incompatible pre-existing database is reported as an error (and
// recreated fresh by Open).
func initSchema(db *sql.DB) error {
	ctx, cancel := context.WithTimeout(context.Background(), openTimeout)
	defer cancel()
	if _, err := db.ExecContext(ctx, schema); err != nil {
		return err
	}
	for _, probe := range []string{
		`SELECT success, last_used FROM usage LIMIT 0`,
		`SELECT count, last_used FROM transitions LIMIT 0`,
	} {
		rows, err := db.QueryContext(ctx, probe)
		if err != nil {
			return err
		}
		if err := rows.Close(); err != nil {
			return err
		}
	}
	return nil
}

// Close releases the database handle. It is a no-op on a nil or degraded
// Store.
func (s *Store) Close() error {
	if s == nil || s.db == nil {
		return nil
	}
	return s.db.Close()
}

// Record persists a completed command (RANK-002): successful executions
// (exitCode == 0) increment the success count and refresh recency; failed
// executions refresh recency only. It also records the transition from
// prevSkeleton to skeleton when prevSkeleton is non-empty and different.
// Recording is best-effort: errors are swallowed so learning never breaks
// the shell.
func (s *Store) Record(cwd, skeleton, prevSkeleton string, exitCode int) {
	if s == nil || s.db == nil || skeleton == "" {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), queryTimeout)
	defer cancel()
	now := time.Now().Unix()
	if exitCode == 0 {
		_, _ = s.db.ExecContext(ctx, `
			INSERT INTO usage (cwd, skeleton, success, last_used) VALUES (?, ?, 1, ?)
			ON CONFLICT (cwd, skeleton) DO UPDATE SET
				success = success + 1,
				last_used = excluded.last_used`,
			cwd, skeleton, now)
	} else {
		_, _ = s.db.ExecContext(ctx, `
			INSERT INTO usage (cwd, skeleton, success, last_used) VALUES (?, ?, 0, ?)
			ON CONFLICT (cwd, skeleton) DO UPDATE SET
				last_used = excluded.last_used`,
			cwd, skeleton, now)
	}
	if prevSkeleton != "" && prevSkeleton != skeleton {
		_, _ = s.db.ExecContext(ctx, `
			INSERT INTO transitions (cwd, prev, cur, count, last_used) VALUES (?, ?, ?, 1, ?)
			ON CONFLICT (cwd, prev, cur) DO UPDATE SET
				count = count + 1,
				last_used = excluded.last_used`,
			cwd, prevSkeleton, skeleton, now)
	}
}

// Frecency returns the raw frecency for skeleton: the exact current-CWD value
// when a row exists, else the global aggregate at 70% strength (RANK-004).
// The value is the recency bucket (RANK-003) multiplied by the success count;
// it is 0 when the skeleton was never seen (or never succeeded).
func (s *Store) Frecency(cwd, skeleton string) float64 {
	if s == nil || s.db == nil || skeleton == "" {
		return 0
	}
	ctx, cancel := context.WithTimeout(context.Background(), queryTimeout)
	defer cancel()
	var success, lastUsed int64
	err := s.db.QueryRowContext(ctx,
		`SELECT success, last_used FROM usage WHERE cwd = ? AND skeleton = ?`,
		cwd, skeleton).Scan(&success, &lastUsed)
	if err == nil {
		return float64(frecencyBucket(ageOf(lastUsed))) * float64(success)
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return 0
	}
	var total, maxUsed sql.NullInt64
	err = s.db.QueryRowContext(ctx,
		`SELECT SUM(success), MAX(last_used) FROM usage WHERE skeleton = ?`,
		skeleton).Scan(&total, &maxUsed)
	if err != nil || !total.Valid || total.Int64 == 0 || !maxUsed.Valid {
		return 0
	}
	return globalFallbackScale * float64(frecencyBucket(ageOf(maxUsed.Int64))) * float64(total.Int64)
}

// Transition returns the learned likelihood (count) of cur following prev:
// current-CWD transitions first, else the global aggregate at 70% strength
// (RANK-005). When no rows exist for the exact previous skeleton, a deep
// previous skeleton falls back to its parent skeleton ("git commit" → "git").
func (s *Store) Transition(cwd, prev, cur string) float64 {
	if s == nil || s.db == nil || prev == "" || cur == "" {
		return 0
	}
	ctx, cancel := context.WithTimeout(context.Background(), queryTimeout)
	defer cancel()
	if v := s.transition(ctx, cwd, prev, cur); v > 0 {
		return v
	}
	if parent := parentSkeleton(prev); parent != prev {
		return s.transition(ctx, cwd, parent, cur)
	}
	return 0
}

func (s *Store) transition(ctx context.Context, cwd, prev, cur string) float64 {
	var count int64
	err := s.db.QueryRowContext(ctx,
		`SELECT count FROM transitions WHERE cwd = ? AND prev = ? AND cur = ?`,
		cwd, prev, cur).Scan(&count)
	if err == nil {
		return float64(count)
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return 0
	}
	var total sql.NullInt64
	err = s.db.QueryRowContext(ctx,
		`SELECT SUM(count) FROM transitions WHERE prev = ? AND cur = ?`,
		prev, cur).Scan(&total)
	if err != nil || !total.Valid || total.Int64 == 0 {
		return 0
	}
	return globalFallbackScale * float64(total.Int64)
}

// Prune forgets rows not used within the last 90 days. It is intended to be
// called occasionally and is cheap.
func (s *Store) Prune() error {
	if s == nil || s.db == nil {
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), queryTimeout)
	defer cancel()
	cutoff := time.Now().Add(-pruneAge).Unix()
	if _, err := s.db.ExecContext(ctx, `DELETE FROM usage WHERE last_used < ?`, cutoff); err != nil {
		return err
	}
	if _, err := s.db.ExecContext(ctx, `DELETE FROM transitions WHERE last_used < ?`, cutoff); err != nil {
		return err
	}
	return nil
}

// frecencyBucket maps a last-used age to its bucket weight (RANK-003).
func frecencyBucket(age time.Duration) int64 {
	switch {
	case age <= time.Hour:
		return 100
	case age <= 24*time.Hour:
		return 50
	case age <= 7*24*time.Hour:
		return 20
	case age <= 30*24*time.Hour:
		return 5
	default:
		return 1
	}
}

func ageOf(lastUsedUnix int64) time.Duration {
	return time.Since(time.Unix(lastUsedUnix, 0))
}

// parentSkeleton reduces a skeleton to its first token: "git commit" → "git".
func parentSkeleton(skeleton string) string {
	if i := strings.IndexByte(skeleton, ' '); i > 0 {
		return skeleton[:i]
	}
	return skeleton
}
