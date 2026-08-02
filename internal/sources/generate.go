package sources

import (
	"context"
	"strings"

	"github.com/rselbach/argmax/internal/core"
)

// GenRequest describes one dynamic generator invocation.
type GenRequest struct {
	ID      string   // generator ID (fixed registry, PRD 9.8)
	RootCmd string   // root command name as resolved (e.g. "docker" or "podman")
	Args    []string // completed positional args at the active node
	Partial string   // current partial token
	CWD     string   // child shell CWD
}

// knownGenerators is the fixed dynamic generator registry (PRD 9.8). The
// file generators ("files", "dirs", "ext:<csv>") are handled separately.
var knownGenerators = map[string]bool{
	"git-branches":              true,
	"git-remotes":               true,
	"git-tags":                  true,
	"git-commits":               true,
	"git-stashes":               true,
	"git-pushpull":              true,
	"git-checkout":              true,
	"git-reset":                 true,
	"git-show":                  true,
	"docker-containers-all":     true,
	"docker-containers-running": true,
	"docker-images":             true,
	"docker-inspect":            true,
	"ssh-hosts":                 true,
	"node-scripts":              true,
	"just-recipes":              true,
	"make-targets":              true,
	"zoxide-dirs":               true,
	"packages-installed":        true,
	"pip-packages":              true,
	"processes":                 true,
	"process-names":             true,
	"env-vars":                  true,
	"chmod-modes":               true,
}

// KnownGenerator reports whether id is a registered dynamic generator ID.
func KnownGenerator(id string) bool {
	if id == "files" || id == "dirs" {
		return true
	}
	if strings.HasPrefix(id, "ext:") {
		return len(id) > len("ext:")
	}
	return knownGenerators[id]
}

// Generate runs the requested dynamic generator. Unknown IDs and probe
// failures return nil.
func (s *Sources) Generate(ctx context.Context, req GenRequest) []core.Suggestion {
	if ctx == nil {
		ctx = context.Background()
	}
	showHidden := s.config().UI.HiddenFiles
	switch {
	case req.ID == "files":
		return s.CompleteFiles(FileRequest{Partial: req.Partial, CWD: req.CWD, Mode: FileAny, ShowHidden: showHidden})
	case req.ID == "dirs":
		return s.CompleteFiles(FileRequest{Partial: req.Partial, CWD: req.CWD, Mode: FileDir, ShowHidden: showHidden})
	case strings.HasPrefix(req.ID, "ext:"):
		exts := strings.Split(req.ID[len("ext:"):], ",")
		if len(exts) == 0 || exts[0] == "" {
			return nil
		}
		return s.CompleteFiles(FileRequest{Partial: req.Partial, CWD: req.CWD, Mode: FileExt, Exts: exts, ShowHidden: showHidden})
	}

	switch req.ID {
	case "git-branches":
		return s.gitBranchesGen(ctx, req)
	case "git-remotes":
		return s.gitRemotesGen(ctx, req)
	case "git-tags":
		return s.gitTagsGen(ctx, req)
	case "git-commits":
		return s.gitCommitsGen(ctx, req)
	case "git-stashes":
		return s.gitStashesGen(ctx, req)
	case "git-pushpull":
		return s.gitPushPullGen(ctx, req)
	case "git-checkout", "git-reset":
		return s.gitMixedGen(ctx, req)
	case "git-show":
		return s.gitShowGen(ctx, req)
	case "docker-containers-all":
		return s.dockerContainersGen(ctx, req, true)
	case "docker-containers-running":
		return s.dockerContainersGen(ctx, req, false)
	case "docker-images":
		return s.dockerImagesGen(ctx, req)
	case "docker-inspect":
		return s.dockerInspectGen(ctx, req)
	case "ssh-hosts":
		return s.sshHostsGen(req)
	case "node-scripts":
		return s.nodeScriptsGen(req)
	case "just-recipes":
		return s.justRecipesGen(req)
	case "make-targets":
		return s.makeTargetsGen(req)
	case "zoxide-dirs":
		return s.zoxideDirsGen(ctx, req)
	case "packages-installed":
		return s.packagesInstalledGen(ctx, req)
	case "pip-packages":
		return s.pipPackagesGen(ctx, req)
	case "processes":
		return s.processesGen(ctx, req)
	case "process-names":
		return s.processNamesGen(ctx, req)
	case "env-vars":
		return s.envVarsGen(req)
	case "chmod-modes":
		return s.chmodModesGen(req)
	}
	return nil
}

// genIcon picks the suggestion icon matching the generator root.
func genIcon(id, root string) string {
	switch {
	case strings.HasPrefix(id, "git-"):
		return "git"
	case strings.HasPrefix(id, "docker-"):
		return root // "docker" or "podman"
	case id == "ssh-hosts":
		return "ssh"
	case id == "node-scripts":
		return "node"
	default:
		return "misc"
	}
}

// dyn builds a dynamic-source suggestion at the default confidence.
func dyn(text, desc, icon string) core.Suggestion {
	return dynConf(text, desc, icon, 70)
}

func dynConf(text, desc, icon string, conf int) core.Suggestion {
	return core.Suggestion{
		Text:        text,
		Description: desc,
		Icon:        icon,
		Source:      core.SourceDynamic,
		Confidence:  conf,
		Priority:    -1,
	}
}
