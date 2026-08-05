package config

import (
	"context"
	"os"
	"sync"
	"time"
)

// Watcher polls the configuration file at most once per second and applies
// valid changes atomically. A malformed reload retains the last valid
// configuration.
type Watcher struct {
	path string

	mu      sync.RWMutex
	current *Config
	modTime time.Time

	// OnChange, when set, is called with the new valid configuration.
	OnChange func(*Config)
	// OnError, when set, is called when a reload fails.
	OnError func(error)
}

// NewWatcher returns a watcher seeded with the given configuration.
func NewWatcher(path string, initial *Config) *Watcher {
	w := &Watcher{path: path, current: initial}
	if info, err := os.Stat(path); err == nil {
		w.modTime = info.ModTime()
	}
	return w
}

// Current returns the last valid configuration.
func (w *Watcher) Current() *Config {
	w.mu.RLock()
	defer w.mu.RUnlock()
	return w.current
}

// Refresh forces an immediate poll, applying any pending file change
// without waiting for the next tick.
func (w *Watcher) Refresh() {
	w.poll()
}

// Run polls until ctx is canceled.
func (w *Watcher) Run(ctx context.Context) {
	ticker := time.NewTicker(time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			w.poll()
		}
	}
}

func (w *Watcher) poll() {
	info, err := os.Stat(w.path)
	if err != nil {
		return
	}
	w.mu.RLock()
	unchanged := !info.ModTime().After(w.modTime)
	w.mu.RUnlock()
	if unchanged {
		return
	}
	cfg, err := Load(w.path)
	if err != nil {
		if w.OnError != nil {
			w.OnError(err)
		}
		return
	}
	w.mu.Lock()
	w.current = cfg
	w.modTime = info.ModTime()
	w.mu.Unlock()
	if w.OnChange != nil {
		w.OnChange(cfg)
	}
}
