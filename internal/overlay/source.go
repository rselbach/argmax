package overlay

import "github.com/rselbach/argmax/internal/core"

// SourceLabel returns the short human label for a source, e.g. "alias",
// "history", "system" (UI-011).
func SourceLabel(src core.Source) string {
	switch src {
	case core.SourceSpec:
		return "spec"
	case core.SourceAlias, core.SourceToolAlias:
		return "alias"
	case core.SourceSystem:
		return "system"
	case core.SourceHistory:
		return "history"
	case core.SourceInferred:
		return "inferred"
	case core.SourceAI:
		return "ai"
	case core.SourceFile:
		return "file"
	case core.SourceDynamic:
		return "live"
	default:
		return string(src)
	}
}
