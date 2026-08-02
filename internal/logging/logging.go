// Package logging provides the structured diagnostic log with size-based
// rotation. Logs never include API keys or authorization headers.
package logging

import (
	"fmt"
	"io"
	"log/slog"
	"os"
	"strings"
	"sync"

	"github.com/rselbach/argmax/internal/paths"
)

const maxLogSize = 5 << 20 // 5 MiB before rotating to .old

var (
	mu     sync.Mutex
	logger = slog.New(slog.NewTextHandler(io.Discard, nil))
	file   *os.File
)

// Setup opens the main log file in the cache directory. level accepts
// debug, info, warn, or error; ARGMAX_LOG_LEVEL overrides it.
func Setup(debug bool) error {
	mu.Lock()
	defer mu.Unlock()
	if err := paths.EnsureDir(paths.CacheDir()); err != nil {
		return fmt.Errorf("create cache dir: %w", err)
	}
	path := paths.Log()
	rotate(path)
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return fmt.Errorf("open log: %w", err)
	}
	file = f
	logger = slog.New(slog.NewTextHandler(f, &slog.HandlerOptions{
		Level:     level(debug),
		AddSource: true,
	}))
	return nil
}

func level(debug bool) slog.Level {
	switch strings.ToLower(os.Getenv("ARGMAX_LOG_LEVEL")) {
	case "debug":
		return slog.LevelDebug
	case "info":
		return slog.LevelInfo
	case "warn":
		return slog.LevelWarn
	case "error":
		return slog.LevelError
	}
	if debug {
		return slog.LevelDebug
	}
	return slog.LevelInfo
}

func rotate(path string) {
	info, err := os.Stat(path)
	if err != nil || info.Size() < maxLogSize {
		return
	}
	_ = os.Rename(path, path+".old")
}

// L returns the process logger.
func L() *slog.Logger {
	mu.Lock()
	defer mu.Unlock()
	return logger
}

// Close flushes and closes the log file.
func Close() {
	mu.Lock()
	defer mu.Unlock()
	if file != nil {
		_ = file.Close()
		file = nil
	}
}
