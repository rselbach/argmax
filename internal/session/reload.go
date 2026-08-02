package session

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// sessionInfo is published for `argmax reload` discovery (RUN-013).
type sessionInfo struct {
	PID     int    `json:"pid"`
	PPID    int    `json:"ppid"`
	Shell   string `json:"shell"`
	Version string `json:"version"`
}

func (s *sess) writeSessionFile() {
	info := sessionInfo{
		PID:     os.Getpid(),
		PPID:    os.Getppid(),
		Shell:   string(s.sh),
		Version: s.version,
	}
	data, err := json.Marshal(info)
	if err != nil {
		return
	}
	_ = os.WriteFile(s.sessionFilePath(), data, 0o600)
}

func (s *sess) removeSessionFile() {
	_ = os.Remove(s.sessionFilePath())
}

func (s *sess) sessionFilePath() string {
	return filepath.Join(s.paths.CacheDir, fmt.Sprintf("session-%d.json", os.Getpid()))
}
