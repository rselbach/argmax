package overlay

import "strings"

// icons maps canonical icon/category keys to Nerd Font glyphs (Private Use
// Area), drawn from the devicons, Font Awesome, Octicons and Material Design
// sets bundled in Nerd Fonts.
var icons = map[string]rune{
	"git":        '\U000f02a2', // md-git
	"github":     '\uf113',     // fa-github
	"docker":     '\ue7b0',     // dev-docker
	"kubernetes": '\U000f10fe', // md-kubernetes
	"cloud":      '\uf0c2',     // fa-cloud
	"database":   '\uf1c0',     // fa-database
	"node":       '\U000f0399', // md-nodejs
	"python":     '\ue73c',     // dev-python
	"rust":       '\ue7a8',     // dev-rust
	"go":         '\ue724',     // dev-go
	"java":       '\ue738',     // dev-java
	"c":          '\ue61e',     // dev-c
	"build":      '\uf0ad',     // fa-wrench
	"package":    '\uf487',     // oct-package
	"fs":         '\uf0a0',     // fa-hdd
	"archive":    '\uf187',     // fa-archive
	"editor":     '\uf044',     // fa-edit
	"viewer":     '\uf06e',     // fa-eye
	"text":       '\uf0f6',     // fa-file-text
	"json":       '\U000f0626', // md-code-json
	"task":       '\uf0ae',     // fa-tasks
	"sysadmin":   '\uf085',     // fa-cogs
	"network":    '\uf0ac',     // fa-globe
	"process":    '\uf2db',     // fa-microchip
	"shell":      '\uf120',     // fa-terminal
	"search":     '\uf002',     // fa-search
	"vcs":        '\uf418',     // oct-git-branch
	"ai":         '\U000f06a9', // md-robot
	"alias":      '\uf0c1',     // fa-link
	"history":    '\uf1da',     // fa-history
	"system":     '\uf013',     // fa-cog
	"file":       '\uf016',     // fa-file
	"directory":  '\uf114',     // fa-folder
	"misc":       '\uf059',     // fa-question-circle (neutral fallback)
}

// IconFor returns the display glyph for an icon/category key. When nerd is
// false it returns "" (the caller drops the glyph column but keeps source
// text and selection state — UI-010/016). Unknown keys return the neutral
// "misc" fallback glyph.
func IconFor(key string, nerd bool) string {
	if !nerd {
		return ""
	}
	if r, ok := icons[strings.ToLower(key)]; ok {
		return string(r)
	}
	return string(icons["misc"])
}
