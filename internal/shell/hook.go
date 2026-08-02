package shell

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// Managed block markers (SH-008). Everything between them, inclusive, is
// owned by argmax: `argmax setup` installs or upgrades it and
// `argmax uninstall` removes exactly that region (UN-001).
const (
	BlockBegin = "# >>> argmax >>>"
	BlockEnd   = "# <<< argmax <<<"
)

// InstallHook idempotently installs the managed block (the InitScript
// content, wrapped in the BlockBegin/BlockEnd markers) into the shell's
// RCFile. It returns the file path and whether the file changed. A missing
// file is created with mode 0600 (parent directories 0755); existing content
// and permissions are preserved. Re-running with an up-to-date block is a
// no-op; an outdated block between the markers is replaced.
func InstallHook(s Shell) (file string, changed bool, err error) {
	file = s.RCFile()
	script := s.InitScript()
	if file == "" || script == "" {
		return "", false, fmt.Errorf("unsupported shell %q", s)
	}
	block := renderBlock(script)

	data, err := os.ReadFile(file)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		if err := os.MkdirAll(filepath.Dir(file), 0o755); err != nil {
			return file, false, fmt.Errorf("create %s: %w", filepath.Dir(file), err)
		}
		if err := os.WriteFile(file, block, 0o600); err != nil {
			return file, false, fmt.Errorf("write %s: %w", file, err)
		}
		return file, true, nil
	case err != nil:
		return file, false, fmt.Errorf("read %s: %w", file, err)
	}

	lines, trailingNL := splitLines(data)
	begin, end, err := findBlock(lines)
	if err != nil {
		return file, false, fmt.Errorf("%s: %w", file, err)
	}

	var out []byte
	if begin >= 0 {
		if strings.Join(lines[begin+1:end], "\n") == strings.TrimRight(script, "\n") {
			return file, false, nil // block present and current
		}
		// Replace the outdated block in place.
		kept := make([]string, 0, len(lines))
		kept = append(kept, lines[:begin]...)
		kept = append(kept, blockLines(block)...)
		kept = append(kept, lines[end+1:]...)
		// If the block was the last thing in the file, make sure the
		// rewritten file still ends with a newline.
		out = joinLines(kept, trailingNL || end == len(lines)-1)
	} else {
		// Append: one blank separator line, then the block.
		out = data
		if len(out) > 0 && out[len(out)-1] != '\n' {
			out = append(out, '\n')
		}
		if len(out) > 0 {
			out = append(out, '\n')
		}
		out = append(out, block...)
	}

	// WriteFile keeps the existing file's permissions; the perm argument
	// only applies if the file vanished between read and write.
	mode := fs.FileMode(0o600)
	if info, statErr := os.Stat(file); statErr == nil {
		mode = info.Mode().Perm()
	}
	if err := os.WriteFile(file, out, mode); err != nil {
		return file, false, fmt.Errorf("write %s: %w", file, err)
	}
	return file, true, nil
}

// RemoveHook deletes only the marked block (UN-001), preserving all
// unrelated content and the file's permissions. It reports changed=false
// when the file or the block does not exist.
func RemoveHook(s Shell) (file string, changed bool, err error) {
	file = s.RCFile()
	if file == "" {
		return "", false, fmt.Errorf("unsupported shell %q", s)
	}
	data, err := os.ReadFile(file)
	switch {
	case errors.Is(err, fs.ErrNotExist):
		return file, false, nil
	case err != nil:
		return file, false, fmt.Errorf("read %s: %w", file, err)
	}

	lines, trailingNL := splitLines(data)
	begin, end, err := findBlock(lines)
	if err != nil {
		return file, false, fmt.Errorf("%s: %w", file, err)
	}
	if begin < 0 {
		return file, false, nil
	}

	// Cut the block plus at most one adjacent blank separator line.
	start, stop := begin, end
	if start > 0 && strings.TrimSpace(lines[start-1]) == "" {
		start--
	} else if stop+1 < len(lines) && strings.TrimSpace(lines[stop+1]) == "" {
		stop++
	}
	kept := make([]string, 0, len(lines)-(stop-start+1))
	kept = append(kept, lines[:start]...)
	kept = append(kept, lines[stop+1:]...)

	// WriteFile keeps the existing file's permissions.
	if err := os.WriteFile(file, joinLines(kept, trailingNL), 0o600); err != nil {
		return file, false, fmt.Errorf("write %s: %w", file, err)
	}
	return file, true, nil
}

// renderBlock wraps the integration script in the managed markers. The
// result always ends with a newline.
func renderBlock(script string) []byte {
	var b strings.Builder
	b.WriteString(BlockBegin)
	b.WriteByte('\n')
	b.WriteString(strings.TrimRight(script, "\n"))
	b.WriteByte('\n')
	b.WriteString(BlockEnd)
	b.WriteByte('\n')
	return []byte(b.String())
}

// blockLines splits a rendered block back into lines for splicing.
func blockLines(block []byte) []string {
	lines, _ := splitLines(block)
	return lines
}

// findBlock locates the managed block, returning the inclusive line indexes
// of the begin and end markers, or -1/-1 when no begin marker exists. A
// begin marker without a matching end marker is an error so that malformed
// files are surfaced instead of silently rewritten.
func findBlock(lines []string) (begin, end int, err error) {
	begin = -1
	for i, line := range lines {
		if strings.TrimSpace(line) == BlockBegin {
			begin = i
			break
		}
	}
	if begin < 0 {
		return -1, -1, nil
	}
	for i := begin + 1; i < len(lines); i++ {
		if strings.TrimSpace(lines[i]) == BlockEnd {
			return begin, i, nil
		}
	}
	return -1, -1, fmt.Errorf("found %q without a matching %q", BlockBegin, BlockEnd)
}

// splitLines splits data into lines without their terminators, reporting
// whether the data ended with a newline.
func splitLines(data []byte) (lines []string, trailingNewline bool) {
	s := string(data)
	trailingNewline = s == "" || strings.HasSuffix(s, "\n")
	s = strings.TrimSuffix(s, "\n")
	if s == "" {
		return nil, trailingNewline
	}
	return strings.Split(s, "\n"), trailingNewline
}

// joinLines is the inverse of splitLines.
func joinLines(lines []string, trailingNewline bool) []byte {
	if len(lines) == 0 {
		return nil
	}
	s := strings.Join(lines, "\n")
	if trailingNewline {
		s += "\n"
	}
	return []byte(s)
}
