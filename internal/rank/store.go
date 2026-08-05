// Package rank implements adaptive ranking: SQLite-backed frecency and
// command-skeleton transitions, workspace signature detection, and the
// deterministic weighted scorer.
package rank

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"time"

	_ "modernc.org/sqlite"

	"github.com/rselbach/argmax/internal/paths"
)

// Store persists command usage and skeleton transitions in a private local
// SQLite database (WAL mode, one process-local connection, five-second
// busy timeout). All errors degrade to deterministic static ranking.
type Store struct {
	db *sql.DB
}

// Open creates or opens the learning database with mode 0600.
func Open() (*Store, error) {
	dir := paths.DataDir()
	if err := paths.EnsureDir(dir); err != nil {
		return nil, fmt.Errorf("create data dir: %w", err)
	}
	path := paths.Database()
	dsn := "file:" + path + "?_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)"
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open database: %w", err)
	}
	db.SetMaxOpenConns(1)
	if _, err := db.Exec(schema); err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("initialize database: %w", err)
	}
	if err := os.Chmod(path, 0o600); err != nil && !os.IsNotExist(err) {
		_ = db.Close()
		return nil, fmt.Errorf("restrict database permissions: %w", err)
	}
	return &Store{db: db}, nil
}

const schema = `
CREATE TABLE IF NOT EXISTS frecency (
	command TEXT NOT NULL,
	cwd TEXT NOT NULL,
	success_count INTEGER NOT NULL DEFAULT 0,
	last_used INTEGER NOT NULL,
	PRIMARY KEY (command, cwd)
);
CREATE TABLE IF NOT EXISTS transitions (
	prev TEXT NOT NULL,
	next TEXT NOT NULL,
	cwd TEXT NOT NULL,
	count INTEGER NOT NULL DEFAULT 0,
	last_used INTEGER NOT NULL,
	PRIMARY KEY (prev, next, cwd)
);
CREATE INDEX IF NOT EXISTS frecency_cwd ON frecency (cwd, last_used DESC);
CREATE INDEX IF NOT EXISTS transitions_prev ON transitions (prev, cwd);
`

// Close releases the database connection.
func (s *Store) Close() error {
	if s == nil || s.db == nil {
		return nil
	}
	return s.db.Close()
}

// Record persists one completed command. Successful executions increment
// the success count; failures refresh recency only. prevSkeleton, when
// non-empty, also records a skeleton transition.
func (s *Store) Record(ctx context.Context, command, skeleton, prevSkeleton, cwd string, exitCode int) error {
	if s == nil || command == "" {
		return nil
	}
	now := time.Now().Unix()
	increment := 0
	if exitCode == 0 {
		increment = 1
	}
	if _, err := s.db.ExecContext(ctx, `
		INSERT INTO frecency (command, cwd, success_count, last_used) VALUES (?, ?, ?, ?)
		ON CONFLICT (command, cwd) DO UPDATE SET
			success_count = success_count + excluded.success_count,
			last_used = excluded.last_used`,
		command, cwd, increment, now); err != nil {
		return fmt.Errorf("record frecency: %w", err)
	}
	if prevSkeleton == "" || skeleton == "" || exitCode != 0 {
		return nil
	}
	if _, err := s.db.ExecContext(ctx, `
		INSERT INTO transitions (prev, next, cwd, count, last_used) VALUES (?, ?, ?, 1, ?)
		ON CONFLICT (prev, next, cwd) DO UPDATE SET
			count = count + 1,
			last_used = excluded.last_used`,
		prevSkeleton, skeleton, cwd, now); err != nil {
		return fmt.Errorf("record transition: %w", err)
	}
	return nil
}

// recencyBucket implements the documented frecency buckets.
func recencyBucket(lastUsed int64, now time.Time) float64 {
	age := now.Sub(time.Unix(lastUsed, 0))
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

// Frecency returns raw frecency weights by command, querying the exact
// CWD first and global aggregates at 70% strength as fallback.
func (s *Store) Frecency(ctx context.Context, cwd string) (map[string]float64, error) {
	if s == nil {
		return nil, nil
	}
	now := time.Now()
	out := map[string]float64{}
	rows, err := s.db.QueryContext(ctx, `
		SELECT command, success_count, last_used FROM frecency
		WHERE cwd = ? ORDER BY last_used DESC LIMIT 500`, cwd)
	if err != nil {
		return nil, fmt.Errorf("query frecency: %w", err)
	}
	if err := scanFrecency(rows, out, 1.0, now); err != nil {
		return nil, err
	}
	global, err := s.db.QueryContext(ctx, `
		SELECT command, SUM(success_count), MAX(last_used) FROM frecency
		GROUP BY command ORDER BY MAX(last_used) DESC LIMIT 500`)
	if err != nil {
		return nil, fmt.Errorf("query global frecency: %w", err)
	}
	if err := scanFrecency(global, out, 0.7, now); err != nil {
		return nil, err
	}
	return out, nil
}

func scanFrecency(rows *sql.Rows, out map[string]float64, strength float64, now time.Time) error {
	defer func() { _ = rows.Close() }()
	for rows.Next() {
		var (
			command  string
			count    int64
			lastUsed int64
		)
		if err := rows.Scan(&command, &count, &lastUsed); err != nil {
			return fmt.Errorf("scan frecency: %w", err)
		}
		score := float64(count) * recencyBucket(lastUsed, now) * strength
		if score > out[command] {
			out[command] = score
		}
	}
	return rows.Err()
}

// Transitions returns raw transition weights for skeletons following
// prevSkeleton: current-CWD evidence first, then global at 70% strength,
// falling back from a deep previous skeleton to its parent at reduced
// strength.
func (s *Store) Transitions(ctx context.Context, prevSkeleton, cwd string, parentOf func(string) string) (map[string]float64, error) {
	if s == nil || prevSkeleton == "" {
		return nil, nil
	}
	out := map[string]float64{}
	strength := 1.0
	for prev := prevSkeleton; prev != ""; prev = parentOf(prev) {
		if err := s.transitionsFor(ctx, prev, cwd, strength, out); err != nil {
			return nil, err
		}
		if len(out) > 0 {
			break
		}
		strength *= 0.5
	}
	return out, nil
}

func (s *Store) transitionsFor(ctx context.Context, prev, cwd string, strength float64, out map[string]float64) error {
	rows, err := s.db.QueryContext(ctx, `
		SELECT next, cwd, count FROM transitions WHERE prev = ?
		ORDER BY (cwd = ?) DESC, count DESC, last_used DESC, next ASC, cwd ASC
		LIMIT 500`, prev, cwd)
	if err != nil {
		return fmt.Errorf("query transitions: %w", err)
	}
	defer func() { _ = rows.Close() }()
	for rows.Next() {
		var (
			next   string
			rowCWD string
			count  int64
		)
		if err := rows.Scan(&next, &rowCWD, &count); err != nil {
			return fmt.Errorf("scan transition: %w", err)
		}
		w := float64(count) * strength
		if rowCWD != cwd {
			w *= 0.7
		}
		if w > out[next] {
			out[next] = w
		}
	}
	return rows.Err()
}
