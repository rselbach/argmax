package spec

// jvmCount is the PRD 18.6 category size (Java, Kotlin, and JVM build
// tools).
const jvmCount = 14

func catalogJVM() []*Spec {
	return []*Spec{
		{
			Name:        "clojure",
			Description: "Clojure CLI",
			Icon:        "java",
			Options: []Option{
				optD("Run with the default aliases", "-M"),
				optVal("Execute a function", "-X"),
				optVal("Execute a tool", "-T"),
			},
		},
		{
			Name:        "dart",
			Description: "Dart language SDK",
			Icon:        "misc",
			Subcommands: []*Spec{
				cmd("run", "Run a Dart program", "misc"),
				cmd("test", "Run tests", "misc"),
				cmd("pub", "Manage packages", "misc"),
				cmd("create", "Create a project", "misc"),
				cmd("analyze", "Analyze the code", "misc"),
				cmd("format", "Format the code", "misc"),
				cmd("fix", "Apply automated fixes", "misc"),
			},
		},
		{
			Name:        "flutter",
			Description: "Flutter SDK",
			Icon:        "misc",
			Subcommands: []*Spec{
				cmd("run", "Run the app", "misc"),
				cmd("build", "Build the app", "misc"),
				cmd("test", "Run tests", "misc"),
				cmd("create", "Create a project", "misc"),
				cmd("pub", "Manage packages", "misc"),
				cmd("analyze", "Analyze the code", "misc"),
				cmd("doctor", "Check the development environment", "misc"),
				cmd("devices", "List connected devices", "misc"),
				cmd("clean", "Clean build artifacts", "misc"),
				cmd("upgrade", "Upgrade Flutter", "misc"),
			},
		},
		{
			Name:        "fvm",
			Description: "Flutter version manager",
			Icon:        "misc",
			Subcommands: []*Spec{
				cmd("install", "Install a Flutter version", "misc"),
				cmd("use", "Select a Flutter version", "misc"),
				cmd("list", "List installed versions", "misc"),
				cmd("global", "Set the global version", "misc"),
				cmd("releases", "List available releases", "misc"),
			},
		},
		{
			Name:        "gradle",
			Description: "Gradle build tool",
			Icon:        "build",
			Options: []Option{
				optGen("dirs", "Project directory", "-p", "--project-dir"),
				optD("Quiet output", "-q", "--quiet"),
				optVal("Console output mode", "--console"),
				optGen("files", "Build file", "-b", "--build-file"),
			},
		},
		{
			Name:        "java",
			Description: "Java runtime launcher",
			Icon:        "java",
			Options: []Option{
				optVal("Class search path", "-cp", "--class-path"),
				optGen("files", "Execute a JAR file", "-jar"),
				optD("Show version", "-version", "--version"),
			},
		},
		{
			Name:        "javac",
			Description: "Java compiler",
			Icon:        "java",
			Options: []Option{
				optGen("dirs", "Class output directory", "-d"),
				optVal("Class search path", "-cp", "--class-path"),
				optD("Show version", "-version"),
			},
			Generator: "files",
		},
		{
			Name:        "jenv",
			Description: "Java version manager",
			Icon:        "java",
			Subcommands: []*Spec{
				cmd("versions", "List installed Java versions", "java"),
				cmd("global", "Set the global Java version", "java"),
				cmd("local", "Set the local Java version", "java"),
				cmd("add", "Add a Java installation", "java"),
				cmd("rehash", "Regenerate shims", "java"),
			},
		},
		{
			Name:        "jmeter",
			Description: "Load testing tool",
			Icon:        "java",
			Options: []Option{
				optD("Run in non-GUI mode", "-n"),
				optGen("files", "Test plan file", "-t"),
				optGen("files", "Log results to file", "-l"),
			},
		},
		cmd("kdoctor", "Kotlin Multiplatform environment diagnostics", "java"),
		{
			Name:        "keytool",
			Description: "Manage keys and certificates",
			Icon:        "java",
			Options: []Option{
				optD("List keystore entries", "-list"),
				optVal("Keystore file", "-keystore"),
				optVal("Entry alias", "-alias"),
				optD("Generate a key pair", "-genkeypair"),
				optD("Export a certificate", "-exportcert"),
				optD("Import a certificate", "-importcert"),
			},
		},
		{
			Name:        "kotlinc",
			Description: "Kotlin compiler",
			Icon:        "java",
			Options: []Option{
				optGen("dirs", "Output directory", "-d"),
				optVal("Class search path", "-classpath"),
				optD("Show version", "-version"),
			},
			Generator: "files",
		},
		{
			Name:        "mvn",
			Description: "Maven build tool",
			Icon:        "build",
			Options: []Option{
				optGen("files", "POM file", "-f", "--file"),
				optVal("Activate profiles", "-P"),
				optVal("Set a system property", "-D"),
				optD("Quiet output", "-q", "--quiet"),
				optD("Work offline", "-o", "--offline"),
				optD("Force update of snapshots", "-U", "--update-snapshots"),
			},
		},
		{
			Name:        "spring",
			Description: "Spring Boot CLI",
			Icon:        "java",
			Subcommands: []*Spec{
				cmd("init", "Create a new project", "java"),
				cmd("run", "Run the application", "java"),
			},
		},
	}
}
