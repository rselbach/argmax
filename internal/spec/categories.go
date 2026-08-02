package spec

// Category groups catalog specs under a PRD 18 category heading.
type Category struct {
	Name  string
	Specs []*Spec
}

// Categories returns the baseline catalog grouped by PRD 18 category, in
// canonical order. The per-placement list includes find twice (filesystem
// and text processing); the registry merges them.
func Categories() []Category {
	return []Category{
		{"Cloud, containers, Kubernetes, DevOps, and databases", catalogCloud()},
		{"JavaScript, TypeScript, frontend, and Node.js", catalogNode()},
		{"Python and data science", catalogPython()},
		{"Rust and modern CLI tools", catalogRust()},
		{"Go development", catalogGo()},
		{"Java, Kotlin, and JVM build tools", catalogJVM()},
		{"C/C++ compilers and build systems", catalogC()},
		{"Git and GitHub tools", catalogGit()},
		{"System package managers", catalogPkg()},
		{"Filesystem, directory, and archive utilities", catalogFS()},
		{"Editors, pagers, and file viewers", catalogEditors()},
		{"Text, JSON, and stream processing", catalogText()},
		{"Task runners and build automation", catalogTask()},
		{"System administration, network, and process management", catalogSysadmin()},
	}
}

// catalog concatenates every category of the baseline catalog (PRD 18).
func catalog() []*Spec {
	cats := Categories()
	specs := make([]*Spec, 0, 600)
	for _, c := range cats {
		specs = append(specs, c.Specs...)
	}
	return specs
}
