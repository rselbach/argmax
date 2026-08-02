// Package logs provides structured diagnostics with rotation and private
// crash reports (PRD 9.15, 9.19).
package logs

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strings"
	"sync"
	"time"
)

const (
	maxLogSize = 5 << 20 // 5 MiB
	stderrKeep = 64 << 10
)

var (
	mu      sync.Mutex
	logger  = slog.New(slog.NewTextHandler(io.Discard, nil))
	closers []io.Closer
)

// Init opens the log file at path and sets the package logger. Level comes
// from ARGMAX_LOG_LEVEL (debug, info, warn, error) with debug forcing the
// debug level. Missing/unwritable paths disable file logging.
func Init(path string, debug bool) error {
	mu.Lock()
	defer mu.Unlock()

	level := slog.LevelWarn
	switch strings.ToLower(os.Getenv("ARGMAX_LOG_LEVEL")) {
	case "debug":
		level = slog.LevelDebug
	case "info":
		level = slog.LevelInfo
	case "warn":
		level = slog.LevelWarn
	case "error":
		level = slog.LevelError
	}
	if debug && level > slog.LevelDebug {
		level = slog.LevelDebug
	}

	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	if err := rotateLocked(path); err != nil {
		_ = f.Close()
		return err
	}
	logger = slog.New(slog.NewTextHandler(f, &slog.HandlerOptions{
		Level:     level,
		AddSource: true,
	}))
	closers = append(closers, f)
	return nil
}

// rotateLocked rolls the log to a single .old file once it exceeds 5 MiB.
func rotateLocked(path string) error {
	st, err := os.Stat(path)
	if err != nil || st.Size() < maxLogSize {
		return nil
	}
	old := path + ".old"
	if err := os.Rename(path, old); err != nil {
		return fmt.Errorf("rotate log: %w", err)
	}
	return nil
}

// Close flushes and closes the log file.
func Close() {
	mu.Lock()
	defer mu.Unlock()
	for _, c := range closers {
		_ = c.Close()
	}
	closers = nil
}

// L returns the package logger. It never logs API keys; callers must not
// attach credential values.
func L() *slog.Logger {
	mu.Lock()
	defer mu.Unlock()
	return logger
}

// Debug logs at debug level.
func Debug(msg string, args ...any) { L().Log(context.Background(), slog.LevelDebug, msg, args...) }

// Info logs at info level.
func Info(msg string, args ...any) { L().Log(context.Background(), slog.LevelInfo, msg, args...) }

// Warn logs at warn level.
func Warn(msg string, args ...any) { L().Log(context.Background(), slog.LevelWarn, msg, args...) }

// Error logs at error level.
func Error(msg string, args ...any) { L().Log(context.Background(), slog.LevelError, msg, args...) }

// WriteCrashReport writes a private timestamped crash report (PRD DIAG-003)
// and returns its path.
func WriteCrashReport(dir, version string, crashErr error, stderrTail []byte) (string, error) {
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", err
	}
	if len(stderrTail) > stderrKeep {
		stderrTail = stderrTail[len(stderrTail)-stderrKeep:]
	}
	name := fmt.Sprintf("crash-%s.txt", time.Now().Format("20060102-150405.000"))
	path := filepath.Join(dir, name)

	var b strings.Builder
	fmt.Fprintf(&b, "argmax crash report\n")
	fmt.Fprintf(&b, "timestamp: %s\n", time.Now().UTC().Format(time.RFC3339Nano))
	fmt.Fprintf(&b, "version: %s\n", version)
	fmt.Fprintf(&b, "os/arch: %s/%s\n", runtime.GOOS, runtime.GOARCH)
	if crashErr != nil {
		fmt.Fprintf(&b, "error: %v\n", crashErr)
	}
	fmt.Fprintf(&b, "\n--- goroutine dump ---\n%s\n", debug.Stack())
	if len(stderrTail) > 0 {
		fmt.Fprintf(&b, "\n--- child stderr (last %d bytes) ---\n%s\n", len(stderrTail), stderrTail)
	}

	if err := os.WriteFile(path, []byte(b.String()), 0o600); err != nil {
		return "", err
	}
	return path, nil
}

// NewestCrash returns the newest crash report path, or "" when none exists.
func NewestCrash(dir string) string {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return ""
	}
	var newest string
	var newestTime time.Time
	for _, e := range entries {
		if e.IsDir() || !strings.HasPrefix(e.Name(), "crash-") {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		if info.ModTime().After(newestTime) {
			newestTime = info.ModTime()
			newest = filepath.Join(dir, e.Name())
		}
	}
	return newest
}

// ClearCrashes removes all stored crash reports and returns the count.
func ClearCrashes(dir string) (int, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return 0, nil
	}
	n := 0
	for _, e := range entries {
		if e.IsDir() || !strings.HasPrefix(e.Name(), "crash-") {
			continue
		}
		if err := os.Remove(filepath.Join(dir, e.Name())); err != nil {
			return n, err
		}
		n++
	}
	return n, nil
}
