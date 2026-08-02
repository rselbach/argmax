// Package sources implements the live candidate sources for argmax:
// PATH executables, shell alias discovery, Git/Cargo tool aliases,
// file/directory completion, dynamic command generators, and Cobra
// __complete inference (PRD 9.6-9.9, 11.3, PERF-005/006).
package sources

import (
	"sync"
	"sync/atomic"

	"github.com/rselbach/argmax/internal/config"
	"github.com/rselbach/argmax/internal/core"
)

// Sources holds the configuration and caches backing the live candidate
// sources. It is safe for concurrent use.
type Sources struct {
	cfg atomic.Pointer[config.Config]

	mu         sync.Mutex
	scanned    bool
	pathNames  []string
	aliasCache map[string]*aliasFileCache
	cobraCache map[string][]core.Suggestion
	cobraOrder []string
}

// New returns a Sources bound to cfg. A nil cfg falls back to the
// compiled defaults.
func New(cfg *config.Config) *Sources {
	if cfg == nil {
		cfg = config.Default()
	}
	s := &Sources{
		aliasCache: make(map[string]*aliasFileCache),
		cobraCache: make(map[string][]core.Suggestion),
	}
	s.cfg.Store(cfg)
	return s
}

// SetConfig swaps the active configuration (live reload).
func (s *Sources) SetConfig(cfg *config.Config) {
	if cfg == nil {
		return
	}
	s.cfg.Store(cfg)
}

// config returns the active configuration.
func (s *Sources) config() *config.Config {
	cfg := s.cfg.Load()
	if cfg == nil {
		return config.Default()
	}
	return cfg
}
