package generators

import (
	"strings"

	"github.com/rselbach/argmax/internal/complete"
)

// DockerContainers suggests container names; running limits to running
// containers.
func DockerContainers(running bool) complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		args := []string{"ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Status}}"}
		if !running {
			args = append(args, "-a")
		}
		var out []complete.Candidate
		for _, ln := range lines(run(ctx.CWD, defaultTimeout, "docker", args...)) {
			fields := strings.SplitN(ln, "\t", 3)
			if len(fields) < 1 || !hasFoldPrefix(fields[0], prefix) {
				continue
			}
			desc := strings.Join(fields[1:], " — ")
			out = append(out, complete.Candidate{
				Title: fields[0], Description: desc, Icon: "docker", Priority: 60,
			})
		}
		return out
	}
}

// DockerImages suggests local image references, omitting dangling images
// and duplicate tags.
func DockerImages() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		var out []complete.Candidate
		seen := map[string]bool{}
		for _, ln := range lines(run(ctx.CWD, defaultTimeout, "docker", "images", "--format", "{{.Repository}}:{{.Tag}}\t{{.Size}}")) {
			ref, size, _ := strings.Cut(ln, "\t")
			if strings.Contains(ref, "<none>") || seen[ref] || !hasFoldPrefix(ref, prefix) {
				continue
			}
			seen[ref] = true
			out = append(out, complete.Candidate{
				Title: ref, Description: "image — " + size, Icon: "docker", Priority: 55,
			})
		}
		return out
	}
}

// DockerInspectTargets combines containers and images.
func DockerInspectTargets() complete.Generator {
	containers := DockerContainers(false)
	images := DockerImages()
	return func(ctx complete.Context, args []string, prefix string) []complete.Candidate {
		out := containers(ctx, args, prefix)
		return append(out, images(ctx, args, prefix)...)
	}
}

// InstalledPackages enumerates installed system packages for
// removal/reinstall-style operations of the given package manager.
func InstalledPackages(manager string) complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		var probe []string
		switch manager {
		case "pacman", "yay", "paru":
			probe = []string{"pacman", "-Qq"}
		case "apt", "apt-get":
			probe = []string{"dpkg-query", "-f", "${binary:Package}\n", "-W"}
		case "dnf", "yum":
			probe = []string{"rpm", "-qa", "--qf", "%{NAME}\n"}
		case "brew":
			probe = []string{"brew", "list", "--formula", "-1"}
		default:
			return nil
		}
		var out []complete.Candidate
		for _, name := range lines(run(ctx.CWD, packageTimeout, probe[0], probe[1:]...)) {
			if !hasFoldPrefix(name, prefix) {
				continue
			}
			out = append(out, complete.Candidate{
				Title: name, Description: "installed package", Icon: "package", Priority: 50,
			})
		}
		return out
	}
}

// PipPackages enumerates installed pip packages with versions, falling
// back from pip to pip3.
func PipPackages() complete.Generator {
	return func(ctx complete.Context, _ []string, prefix string) []complete.Candidate {
		out := run(ctx.CWD, packageTimeout, "pip", "list", "--format", "freeze")
		if out == "" {
			out = run(ctx.CWD, packageTimeout, "pip3", "list", "--format", "freeze")
		}
		var res []complete.Candidate
		for _, ln := range lines(out) {
			name, version, _ := strings.Cut(ln, "==")
			if name == "" || !hasFoldPrefix(name, prefix) {
				continue
			}
			res = append(res, complete.Candidate{
				Title: name, Description: "pip package " + version, Icon: "python", Priority: 50,
			})
		}
		return res
	}
}
