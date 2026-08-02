package spec

// cloudCount is the PRD 18.1 category size (Cloud, containers, Kubernetes,
// DevOps, and databases).
const cloudCount = 118

func catalogCloud() []*Spec {
	specs := []*Spec{
		awsSpec(),
		asdfSpec(),
		containerRuntimeSpec("docker", "Container platform"),
		dockerComposeSpec("docker-compose", "Define and run multi-container applications"),
		gcloudSpec(),
		ghSpec(),
		helmSpec(),
		kubectlSpec(),
		nvmSpec(),
		containerRuntimeSpec("podman", "Daemonless container engine"),
		rbenvSpec(),
		rsyncSpec(),
		scpSpec(),
		sftpSpec(),
		sshKeygenSpec(),
		sshSpec(),
		terraformSpec("terraform", "Infrastructure as code"),
		terraformSpec("terragrunt", "Terraform wrapper for DRY configurations"),
		voltaSpec(),
	}
	for _, e := range cloudSimple {
		specs = append(specs, cmd(e[0], e[1], e[2]))
	}
	return specs
}

// cloudSimple holds the single-level entries of PRD 18.1: name, description,
// icon.
var cloudSimple = [][3]string{
	{"amplify", "AWS Amplify CLI", "cloud"},
	{"ampx", "Amplify code-first DX CLI", "cloud"},
	{"ansible", "Automation and configuration management", "cloud"},
	{"ansible-config", "View Ansible configuration", "cloud"},
	{"ansible-doc", "Show Ansible module documentation", "cloud"},
	{"ansible-galaxy", "Manage Ansible roles and collections", "cloud"},
	{"ansible-lint", "Lint Ansible playbooks", "cloud"},
	{"ansible-playbook", "Run Ansible playbooks", "cloud"},
	{"appwrite", "Appwrite backend server CLI", "cloud"},
	{"arch", "Print the machine architecture", "sysadmin"},
	{"arduino-cli", "Arduino command-line interface", "c"},
	{"argo", "Argo Workflows CLI", "kubernetes"},
	{"atlas", "Database schema management", "database"},
	{"aws-vault", "Store AWS credentials securely", "cloud"},
	{"bit", "Component-based development CLI", "build"},
	{"bosh", "BOSH deployment CLI", "cloud"},
	{"capacitor", "Cross-platform native runtime CLI", "node"},
	{"cdk", "AWS Cloud Development Kit", "cloud"},
	{"cf", "Cloud Foundry CLI", "cloud"},
	{"checkov", "Infrastructure as code scanner", "cloud"},
	{"circleci", "CircleCI CLI", "cloud"},
	{"cloudflared", "Cloudflare Tunnel client", "network"},
	{"coda", "Coda CLI", "cloud"},
	{"command", "Run a command bypassing shell functions", "shell"},
	{"copilot", "AWS Copilot for containers", "cloud"},
	{"cosign", "Sign and verify containers", "cloud"},
	{"dapr", "Distributed application runtime CLI", "cloud"},
	{"datree", "Kubernetes configuration policy check", "kubernetes"},
	{"deployctl", "Deno Deploy CLI", "cloud"},
	{"direnv", "Per-directory environment variables", "shell"},
	{"doctl", "DigitalOcean CLI", "cloud"},
	{"doppler", "Secrets management CLI", "cloud"},
	{"eas", "Expo Application Services CLI", "cloud"},
	{"fastly", "Fastly edge cloud CLI", "cloud"},
	{"firebase", "Firebase CLI", "cloud"},
	{"flyctl", "Fly.io CLI", "cloud"},
	{"fnm", "Fast Node.js version manager", "node"},
	{"gpg", "GNU Privacy Guard encryption", "sysadmin"},
	{"hasura", "Hasura GraphQL engine CLI", "database"},
	{"helmfile", "Declarative Helm chart management", "kubernetes"},
	{"hugo", "Static site generator", "build"},
	{"k3d", "Run k3s clusters in Docker", "kubernetes"},
	{"k6", "Load testing tool", "cloud"},
	{"k9s", "Terminal UI for Kubernetes", "kubernetes"},
	{"kind", "Kubernetes in Docker", "kubernetes"},
	{"knex", "SQL query builder CLI", "database"},
	{"kubectx", "Switch Kubernetes contexts", "kubernetes"},
	{"kubens", "Switch Kubernetes namespaces", "kubernetes"},
	{"limactl", "Linux virtual machines on macOS", "cloud"},
	{"locust", "Load testing tool", "python"},
	{"lpass", "LastPass CLI", "misc"},
	{"minikube", "Local Kubernetes cluster", "kubernetes"},
	{"mongocli", "MongoDB management CLI", "database"},
	{"mongoimport", "Import data into MongoDB", "database"},
	{"mongosh", "MongoDB shell", "database"},
	{"multipass", "Ubuntu virtual machines", "cloud"},
	{"mysql", "MySQL client", "database"},
	{"netlify", "Netlify CLI", "cloud"},
	{"newman", "Postman collection runner", "node"},
	{"nginx", "Web server and reverse proxy", "network"},
	{"ngrok", "Expose local servers publicly", "network"},
	{"oci", "Oracle Cloud Infrastructure CLI", "cloud"},
	{"okteto", "Kubernetes development environments", "kubernetes"},
	{"op", "1Password CLI", "misc"},
	{"opa", "Open Policy Agent", "cloud"},
	{"osqueryi", "Interactive osquery SQL shell", "sysadmin"},
	{"pass", "Standard Unix password manager", "misc"},
	{"pg_dump", "Back up a PostgreSQL database", "database"},
	{"pgcli", "PostgreSQL client with completion", "database"},
	{"pm2", "Node.js process manager", "node"},
	{"pod", "CocoaPods dependency manager", "package"},
	{"pscale", "PlanetScale CLI", "database"},
	{"psql", "PostgreSQL client", "database"},
	{"pulumi", "Infrastructure as code", "cloud"},
	{"qodana", "JetBrains code quality platform", "build"},
	{"railway", "Railway deployment CLI", "cloud"},
	{"robot", "Robot Framework test runner", "task"},
	{"serverless", "Serverless Framework", "cloud"},
	{"sfdx", "Salesforce DX CLI", "cloud"},
	{"space", "Deta Space CLI", "cloud"},
	{"sqlite3", "SQLite database shell", "database"},
	{"src", "Sourcegraph CLI", "search"},
	{"stripe", "Stripe CLI", "cloud"},
	{"supabase", "Supabase CLI", "database"},
	{"surreal", "SurrealDB CLI", "database"},
	{"tailscale", "WireGuard mesh VPN CLI", "network"},
	{"tfenv", "Terraform version manager", "cloud"},
	{"tfsec", "Terraform security scanner", "cloud"},
	{"tkn", "Tekton CLI", "kubernetes"},
	{"trivy", "Container vulnerability scanner", "cloud"},
	{"tsuru", "Tsuru platform as a service CLI", "cloud"},
	{"vault", "HashiCorp Vault secrets management", "cloud"},
	{"vela", "KubeVela application CLI", "kubernetes"},
	{"vercel", "Vercel deployment CLI", "cloud"},
	{"watson", "Time tracking CLI", "task"},
	{"whois", "Query domain registration records", "network"},
	{"wrangler", "Cloudflare Workers CLI", "cloud"},
	{"xc", "Markdown-based task runner", "task"},
	{"xcodes", "Manage Xcode versions", "build"},
}

// containerRuntimeSpec builds the deep spec shared by docker and podman.
// Podman intentionally reuses the docker-* generator IDs; the sources
// package receives the root command name with each generator request and
// probes podman instead of docker as appropriate.
func containerRuntimeSpec(name, desc string) *Spec {
	s := &Spec{
		Name:        name,
		Description: desc,
		Icon:        "docker",
		Options: []Option{
			optD("Show version", "--version"),
			optD("Show help", "--help"),
			optVal("Daemon socket to connect to", "-H", "--host"),
		},
		Subcommands: []*Spec{
			{
				Name: "run", Description: "Run a command in a new container", Icon: "docker",
				Options: []Option{
					optD("Interactive with pseudo-TTY", "-it"),
					optD("Keep STDIN open", "-i", "--interactive"),
					optD("Allocate a pseudo-TTY", "-t", "--tty"),
					optD("Run in the background", "-d", "--detach"),
					optVal("Assign a container name", "--name"),
					optVal("Publish a container port", "-p", "--publish"),
					optVal("Bind mount a volume", "-v", "--volume"),
					optVal("Set an environment variable", "-e", "--env"),
					optD("Remove the container when it exits", "--rm"),
				},
				Generator: "docker-images",
			},
			{
				Name: "exec", Description: "Run a command in a running container", Icon: "docker",
				Options: []Option{
					optD("Interactive with pseudo-TTY", "-it"),
					optD("Keep STDIN open", "-i", "--interactive"),
					optD("Allocate a pseudo-TTY", "-t", "--tty"),
					optVal("Run as the given user", "-u", "--user"),
					optVal("Set the working directory", "-w", "--workdir"),
					optVal("Set an environment variable", "-e", "--env"),
				},
				Generator: "docker-containers-running",
			},
			{
				Name: "ps", Description: "List containers", Icon: "docker",
				Options: []Option{
					optD("Show all containers", "-a", "--all"),
					optD("Show numeric IDs only", "-q", "--quiet"),
					optVal("Pretty-print using a Go template", "--format"),
				},
			},
			{
				Name: "images", Description: "List images", Icon: "docker",
				Options: []Option{
					optD("Show all images", "-a", "--all"),
					optD("Show numeric IDs only", "-q", "--quiet"),
					optVal("Filter output", "--filter"),
				},
			},
			{
				Name: "pull", Description: "Pull an image", Icon: "docker",
				Options: []Option{
					optVal("Pull for a specific platform", "--platform"),
					optD("Pull all tags", "-a", "--all-tags"),
				},
				Generator: "docker-images",
			},
			{Name: "push", Description: "Push an image", Icon: "docker", Generator: "docker-images"},
			{
				Name: "build", Description: "Build an image from a Dockerfile", Icon: "docker",
				Options: []Option{
					optVal("Name and optionally tag the image", "-t", "--tag"),
					optGen("files", "Path to the Dockerfile", "-f", "--file"),
					optD("Do not use the build cache", "--no-cache"),
					optVal("Build for a specific platform", "--platform"),
				},
				Generator: "dirs",
			},
			{
				Name: "stop", Description: "Stop running containers", Icon: "docker",
				Options: []Option{
					optVal("Seconds to wait before killing", "-t", "--time"),
				},
				Generator: "docker-containers-running",
			},
			{Name: "start", Description: "Start stopped containers", Icon: "docker", Generator: "docker-containers-all"},
			{
				Name: "restart", Description: "Restart containers", Icon: "docker",
				Options: []Option{
					optVal("Seconds to wait before killing", "-t", "--time"),
				},
				Generator: "docker-containers-running",
			},
			{
				Name: "rm", Description: "Remove containers", Icon: "docker",
				Options: []Option{
					optD("Force removal of running containers", "-f", "--force"),
					optD("Remove anonymous volumes", "-v", "--volumes"),
				},
				Generator: "docker-containers-all",
			},
			{
				Name: "rmi", Description: "Remove images", Icon: "docker",
				Options: []Option{
					optD("Force removal", "-f", "--force"),
					optD("Do not delete untagged parents", "--no-prune"),
				},
				Generator: "docker-images",
			},
			{
				Name: "logs", Description: "Fetch container logs", Icon: "docker",
				Options: []Option{
					optD("Follow log output", "-f", "--follow"),
					optVal("Number of lines from the end", "--tail"),
					optD("Show timestamps", "-t", "--timestamps"),
					optVal("Show logs since a timestamp", "--since"),
				},
				Generator: "docker-containers-running",
			},
			{
				Name: "inspect", Description: "Show low-level object information", Icon: "docker",
				Options: []Option{
					optVal("Format output using a Go template", "--format"),
				},
				Generator: "docker-inspect",
			},
			{
				Name: "network", Description: "Manage networks", Icon: "docker",
				Subcommands: []*Spec{
					cmd("ls", "List networks", "docker"),
					cmd("create", "Create a network", "docker"),
					{Name: "rm", Aliases: []string{"remove"}, Description: "Remove networks", Icon: "docker"},
					cmd("inspect", "Show network details", "docker"),
					cmd("connect", "Connect a container to a network", "docker"),
					cmd("disconnect", "Disconnect a container from a network", "docker"),
					cmd("prune", "Remove unused networks", "docker"),
				},
			},
			{
				Name: "volume", Description: "Manage volumes", Icon: "docker",
				Subcommands: []*Spec{
					cmd("ls", "List volumes", "docker"),
					cmd("create", "Create a volume", "docker"),
					{Name: "rm", Aliases: []string{"remove"}, Description: "Remove volumes", Icon: "docker"},
					cmd("inspect", "Show volume details", "docker"),
					cmd("prune", "Remove unused volumes", "docker"),
				},
			},
			{
				Name: "system", Description: "Manage the engine", Icon: "docker",
				Subcommands: []*Spec{
					cmd("df", "Show disk usage", "docker"),
					{
						Name: "prune", Description: "Remove unused data", Icon: "docker",
						Options: []Option{
							optD("Remove all unused data", "-a", "--all"),
							optD("Do not prompt for confirmation", "-f", "--force"),
						},
					},
					cmd("info", "Show system-wide information", "docker"),
					cmd("events", "Stream real-time events", "docker"),
				},
			},
			{Name: "tag", Description: "Tag an image", Icon: "docker", Generator: "docker-images"},
			{
				Name: "cp", Description: "Copy files between container and host", Icon: "docker",
				Options: []Option{
					optD("Archive mode", "-a", "--archive"),
					optD("Follow symbolic links", "-L", "--follow-link"),
				},
			},
		},
	}
	if name == "docker" {
		s.Subcommands = append(s.Subcommands, composeSubcommand())
	}
	return s
}

// composeSubcommand builds the static `docker compose` subcommand tree (PRD
// 9.8: static Compose subcommands and YAML file completion).
func composeSubcommand() *Spec {
	return &Spec{
		Name: "compose", Description: "Docker Compose", Icon: "docker",
		Options: []Option{
			optGen("ext:yaml,yml", "Compose configuration file", "-f", "--file"),
			optVal("Project name", "-p", "--project-name"),
		},
		Subcommands: []*Spec{
			{
				Name: "up", Description: "Create and start containers", Icon: "docker",
				Options: []Option{
					optD("Run in the background", "-d", "--detach"),
					optD("Rebuild images before starting", "--build"),
					optD("Recreate containers", "--force-recreate"),
				},
			},
			{
				Name: "down", Description: "Stop and remove containers", Icon: "docker",
				Options: []Option{
					optD("Remove named volumes", "-v", "--volumes"),
					optVal("Remove images", "--rmi"),
				},
			},
			cmd("ps", "List containers", "docker"),
			{
				Name: "logs", Description: "Show service logs", Icon: "docker",
				Options: []Option{
					optD("Follow log output", "-f", "--follow"),
					optVal("Number of lines from the end", "--tail"),
				},
			},
			cmd("exec", "Execute a command in a running container", "docker"),
			cmd("build", "Build or rebuild services", "docker"),
			cmd("stop", "Stop services", "docker"),
			cmd("start", "Start services", "docker"),
			cmd("restart", "Restart services", "docker"),
			cmd("pull", "Pull service images", "docker"),
		},
	}
}

// dockerComposeSpec builds the standalone docker-compose spec with the same
// static subcommands as `docker compose`.
func dockerComposeSpec(name, desc string) *Spec {
	s := composeSubcommand()
	s.Name = name
	s.Description = desc
	return s
}

// awsSpec builds the AWS CLI spec with top-level services.
func awsSpec() *Spec {
	return &Spec{
		Name:        "aws",
		Description: "Amazon Web Services CLI",
		Icon:        "cloud",
		Options: []Option{
			optVal("Use a specific profile", "--profile"),
			optVal("Use a specific region", "--region"),
			optVal("Set the output format", "--output"),
		},
		Subcommands: []*Spec{
			{
				Name: "s3", Description: "S3 object storage commands", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("ls", "List buckets and objects", "cloud"),
					{
						Name: "cp", Description: "Copy objects", Icon: "cloud",
						Options: []Option{
							optD("Copy recursively", "--recursive"),
							optVal("Exclude matching objects", "--exclude"),
							optVal("Set the ACL", "--acl"),
						},
					},
					cmd("mv", "Move objects", "cloud"),
					cmd("rm", "Delete objects", "cloud"),
					cmd("sync", "Synchronize directories and S3 prefixes", "cloud"),
					cmd("mb", "Create a bucket", "cloud"),
					cmd("rb", "Remove a bucket", "cloud"),
				},
			},
			{
				Name: "ec2", Description: "EC2 virtual servers", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("describe-instances", "List EC2 instances", "cloud"),
					cmd("start-instances", "Start EC2 instances", "cloud"),
					cmd("stop-instances", "Stop EC2 instances", "cloud"),
					cmd("reboot-instances", "Reboot EC2 instances", "cloud"),
					cmd("terminate-instances", "Terminate EC2 instances", "cloud"),
					cmd("describe-images", "List machine images", "cloud"),
				},
			},
			{
				Name: "lambda", Description: "AWS Lambda functions", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list-functions", "List functions", "cloud"),
					cmd("invoke", "Invoke a function", "cloud"),
					cmd("get-function", "Show function configuration", "cloud"),
					cmd("create-function", "Create a function", "cloud"),
					cmd("update-function-code", "Update function code", "cloud"),
				},
			},
			{
				Name: "iam", Description: "Identity and access management", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list-users", "List IAM users", "cloud"),
					cmd("list-roles", "List IAM roles", "cloud"),
					cmd("get-user", "Show an IAM user", "cloud"),
					cmd("create-user", "Create an IAM user", "cloud"),
				},
			},
			{
				Name: "eks", Description: "Elastic Kubernetes Service", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list-clusters", "List EKS clusters", "cloud"),
					cmd("describe-cluster", "Show an EKS cluster", "cloud"),
					cmd("update-kubeconfig", "Configure kubectl for a cluster", "cloud"),
				},
			},
			{
				Name: "ecs", Description: "Elastic Container Service", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list-clusters", "List ECS clusters", "cloud"),
					cmd("list-services", "List ECS services", "cloud"),
					cmd("describe-services", "Show ECS services", "cloud"),
				},
			},
			{
				Name: "sts", Description: "Security Token Service", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("get-caller-identity", "Show the current identity", "cloud"),
					cmd("assume-role", "Assume an IAM role", "cloud"),
				},
			},
			{
				Name: "cloudformation", Description: "CloudFormation stacks", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("deploy", "Deploy a stack", "cloud"),
					cmd("create-stack", "Create a stack", "cloud"),
					cmd("update-stack", "Update a stack", "cloud"),
					cmd("delete-stack", "Delete a stack", "cloud"),
					cmd("describe-stacks", "Show stacks", "cloud"),
					cmd("list-stacks", "List stacks", "cloud"),
				},
			},
			{
				Name: "logs", Description: "CloudWatch Logs", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("tail", "Follow log events", "cloud"),
					cmd("describe-log-groups", "List log groups", "cloud"),
					cmd("filter-log-events", "Search log events", "cloud"),
				},
			},
			{
				Name: "sqs", Description: "Simple Queue Service", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list-queues", "List queues", "cloud"),
					cmd("send-message", "Send a message", "cloud"),
					cmd("receive-message", "Receive messages", "cloud"),
				},
			},
			{
				Name: "sns", Description: "Simple Notification Service", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list-topics", "List topics", "cloud"),
					cmd("publish", "Publish a message", "cloud"),
				},
			},
			{
				Name: "dynamodb", Description: "DynamoDB tables", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list-tables", "List tables", "cloud"),
					cmd("describe-table", "Show a table", "cloud"),
					cmd("scan", "Scan a table", "cloud"),
					cmd("query", "Query a table", "cloud"),
				},
			},
			{
				Name: "rds", Description: "Relational Database Service", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("describe-db-instances", "List database instances", "cloud"),
					cmd("create-db-instance", "Create a database instance", "cloud"),
				},
			},
			{
				Name: "configure", Description: "Configure the AWS CLI", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list", "Show configuration", "cloud"),
					cmd("get", "Get a configuration value", "cloud"),
					cmd("set", "Set a configuration value", "cloud"),
				},
			},
		},
	}
}

// gcloudSpec builds the Google Cloud CLI spec with top-level service groups.
func gcloudSpec() *Spec {
	return &Spec{
		Name:        "gcloud",
		Description: "Google Cloud CLI",
		Icon:        "cloud",
		Options: []Option{
			optVal("Use a specific project", "--project"),
			optVal("Use a specific account", "--account"),
			optVal("Set the output format", "--format"),
		},
		Subcommands: []*Spec{
			{
				Name: "compute", Description: "Compute Engine resources", Icon: "cloud",
				Subcommands: []*Spec{
					{
						Name: "instances", Description: "Manage VM instances", Icon: "cloud",
						Subcommands: []*Spec{
							cmd("list", "List instances", "cloud"),
							cmd("start", "Start instances", "cloud"),
							cmd("stop", "Stop instances", "cloud"),
							cmd("reset", "Reset instances", "cloud"),
							cmd("describe", "Show an instance", "cloud"),
						},
					},
					cmd("ssh", "SSH into a VM instance", "cloud"),
					cmd("scp", "Copy files to and from VM instances", "cloud"),
				},
			},
			{
				Name: "container", Description: "Kubernetes Engine resources", Icon: "cloud",
				Subcommands: []*Spec{
					{
						Name: "clusters", Description: "Manage GKE clusters", Icon: "cloud",
						Subcommands: []*Spec{
							cmd("list", "List clusters", "cloud"),
							cmd("describe", "Show a cluster", "cloud"),
							cmd("get-credentials", "Configure kubectl for a cluster", "cloud"),
							cmd("create", "Create a cluster", "cloud"),
							cmd("delete", "Delete a cluster", "cloud"),
							cmd("resize", "Resize a cluster", "cloud"),
						},
					},
				},
			},
			{
				Name: "functions", Description: "Cloud Functions", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("deploy", "Deploy a function", "cloud"),
					cmd("list", "List functions", "cloud"),
					cmd("describe", "Show a function", "cloud"),
					cmd("call", "Invoke a function", "cloud"),
					cmd("delete", "Delete a function", "cloud"),
					cmd("logs", "Show function logs", "cloud"),
				},
			},
			{
				Name: "run", Description: "Cloud Run services", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("deploy", "Deploy a service", "cloud"),
					cmd("list", "List services", "cloud"),
					cmd("describe", "Show a service", "cloud"),
					cmd("delete", "Delete a service", "cloud"),
				},
			},
			{
				Name: "auth", Description: "Authentication", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("login", "Authenticate with user credentials", "cloud"),
					cmd("list", "List credentialed accounts", "cloud"),
					cmd("activate-service-account", "Authenticate with a service account", "cloud"),
					cmd("print-access-token", "Print an access token", "cloud"),
					cmd("application-default", "Manage application default credentials", "cloud"),
				},
			},
			{
				Name: "config", Description: "CLI configuration", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("set", "Set a property", "cloud"),
					cmd("get-value", "Show a property", "cloud"),
					cmd("list", "List properties", "cloud"),
				},
			},
			{
				Name: "projects", Description: "Manage GCP projects", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list", "List projects", "cloud"),
					cmd("describe", "Show a project", "cloud"),
					cmd("create", "Create a project", "cloud"),
					cmd("delete", "Delete a project", "cloud"),
				},
			},
			{
				Name: "app", Description: "App Engine applications", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("deploy", "Deploy an application", "cloud"),
					cmd("browse", "Open the application in a browser", "cloud"),
				},
			},
		},
	}
}

// ghSpec builds the GitHub CLI spec with nested subcommands.
func ghSpec() *Spec {
	return &Spec{
		Name:        "gh",
		Description: "GitHub CLI",
		Icon:        "github",
		Subcommands: []*Spec{
			{
				Name: "pr", Description: "Manage pull requests", Icon: "github",
				Subcommands: []*Spec{
					cmd("create", "Create a pull request", "github"),
					cmd("list", "List pull requests", "github"),
					{Name: "checkout", Description: "Check out a pull request", Icon: "github"},
					cmd("merge", "Merge a pull request", "github"),
					cmd("close", "Close a pull request", "github"),
					cmd("view", "View a pull request", "github"),
					cmd("diff", "Show pull request changes", "github"),
				},
			},
			{
				Name: "issue", Description: "Manage issues", Icon: "github",
				Subcommands: []*Spec{
					cmd("create", "Create an issue", "github"),
					cmd("list", "List issues", "github"),
					cmd("close", "Close an issue", "github"),
					cmd("view", "View an issue", "github"),
				},
			},
			{
				Name: "repo", Description: "Manage repositories", Icon: "github",
				Subcommands: []*Spec{
					cmd("clone", "Clone a repository", "github"),
					cmd("create", "Create a repository", "github"),
					cmd("view", "View a repository", "github"),
					cmd("fork", "Fork a repository", "github"),
				},
			},
			{
				Name: "run", Description: "Manage workflow runs", Icon: "github",
				Subcommands: []*Spec{
					cmd("list", "List workflow runs", "github"),
					cmd("view", "View a workflow run", "github"),
					cmd("watch", "Watch a workflow run", "github"),
				},
			},
			{
				Name: "release", Description: "Manage releases", Icon: "github",
				Subcommands: []*Spec{
					cmd("create", "Create a release", "github"),
					cmd("list", "List releases", "github"),
					cmd("view", "View a release", "github"),
					cmd("upload", "Upload release assets", "github"),
				},
			},
			cmd("browse", "Open the repository in a browser", "github"),
			{
				Name: "auth", Description: "Authentication", Icon: "github",
				Subcommands: []*Spec{
					cmd("login", "Authenticate with GitHub", "github"),
					cmd("logout", "Remove authentication", "github"),
					cmd("status", "Show authentication status", "github"),
				},
			},
			{
				Name: "gist", Description: "Manage gists", Icon: "github",
				Subcommands: []*Spec{
					cmd("create", "Create a gist", "github"),
					cmd("list", "List gists", "github"),
					cmd("view", "View a gist", "github"),
				},
			},
			cmd("api", "Call the GitHub API", "github"),
		},
	}
}

// helmSpec builds the Helm package manager spec.
func helmSpec() *Spec {
	return &Spec{
		Name:        "helm",
		Description: "Kubernetes package manager",
		Icon:        "kubernetes",
		Options: []Option{
			optVal("Kubernetes namespace", "-n", "--namespace"),
			optVal("Set values on the command line", "--set"),
		},
		Subcommands: []*Spec{
			{
				Name: "install", Description: "Install a chart", Icon: "kubernetes",
				Options: []Option{
					optGen("ext:yaml,yml", "Values file", "-f", "--values"),
					optVal("Set values on the command line", "--set"),
					optVal("Chart version", "--version"),
				},
			},
			{
				Name: "upgrade", Description: "Upgrade a release", Icon: "kubernetes",
				Options: []Option{
					optGen("ext:yaml,yml", "Values file", "-f", "--values"),
					optVal("Set values on the command line", "--set"),
					optD("Install the release if absent", "-i", "--install"),
				},
			},
			{Name: "uninstall", Aliases: []string{"delete"}, Description: "Uninstall a release", Icon: "kubernetes"},
			{
				Name: "list", Description: "List releases", Icon: "kubernetes",
				Options: []Option{
					optD("Show all releases", "-a", "--all"),
				},
			},
			{
				Name: "repo", Description: "Manage chart repositories", Icon: "kubernetes",
				Subcommands: []*Spec{
					cmd("add", "Add a chart repository", "kubernetes"),
					cmd("list", "List chart repositories", "kubernetes"),
					cmd("remove", "Remove a chart repository", "kubernetes"),
					cmd("update", "Update repository indexes", "kubernetes"),
					cmd("index", "Generate an index file", "kubernetes"),
				},
			},
			{
				Name: "template", Description: "Render chart templates locally", Icon: "kubernetes",
				Options: []Option{
					optGen("ext:yaml,yml", "Values file", "-f", "--values"),
					optVal("Set values on the command line", "--set"),
				},
			},
			cmd("package", "Package a chart directory", "kubernetes"),
			cmd("lint", "Lint a chart", "kubernetes"),
			{
				Name: "show", Description: "Show chart information", Icon: "kubernetes",
				Subcommands: []*Spec{
					cmd("all", "Show all chart information", "kubernetes"),
					cmd("chart", "Show the chart definition", "kubernetes"),
					cmd("readme", "Show the chart README", "kubernetes"),
					cmd("values", "Show the chart values", "kubernetes"),
				},
			},
			cmd("pull", "Download a chart", "kubernetes"),
			{
				Name: "search", Description: "Search for charts", Icon: "kubernetes",
				Subcommands: []*Spec{
					cmd("repo", "Search configured repositories", "kubernetes"),
					cmd("hub", "Search the Artifact Hub", "kubernetes"),
				},
			},
		},
	}
}

// kubectlSpec builds the Kubernetes CLI spec. kubectl intentionally has no
// live menu generators; resource values come from the AI context (PRD 9.8).
func kubectlSpec() *Spec {
	return &Spec{
		Name:        "kubectl",
		Description: "Kubernetes command-line tool",
		Icon:        "kubernetes",
		Options: []Option{
			optVal("Kubernetes namespace", "-n", "--namespace"),
			optGen("ext:yaml,yml,json", "Manifest file", "-f", "--filename"),
			optVal("Kubeconfig context", "--context"),
			optVal("Output format", "-o", "--output"),
			optD("All namespaces", "-A", "--all-namespaces"),
		},
		Subcommands: []*Spec{
			{
				Name: "get", Description: "Display resources", Icon: "kubernetes",
				Options: []Option{
					optVal("Output format", "-o", "--output"),
					optD("Watch for changes", "-w", "--watch"),
				},
			},
			cmd("describe", "Show resource details", "kubernetes"),
			cmd("apply", "Apply a configuration", "kubernetes"),
			{
				Name: "delete", Description: "Delete resources", Icon: "kubernetes",
				Options: []Option{
					optD("Delete all matching resources", "--all"),
					optD("Force deletion", "--force"),
				},
			},
			{
				Name: "logs", Description: "Print container logs", Icon: "kubernetes",
				Options: []Option{
					optD("Follow log output", "-f", "--follow"),
					optVal("Number of lines from the end", "--tail"),
					optVal("Container name", "-c", "--container"),
					optD("Print previous terminated container logs", "-p", "--previous"),
				},
			},
			{
				Name: "exec", Description: "Execute a command in a container", Icon: "kubernetes",
				Options: []Option{
					optD("Interactive with pseudo-TTY", "-it"),
					optD("Keep STDIN open", "-i", "--stdin"),
					optD("Allocate a pseudo-TTY", "-t", "--tty"),
					optVal("Container name", "-c", "--container"),
				},
			},
			{
				Name: "rollout", Description: "Manage rollouts", Icon: "kubernetes",
				Subcommands: []*Spec{
					cmd("status", "Show rollout status", "kubernetes"),
					cmd("history", "Show rollout history", "kubernetes"),
					cmd("pause", "Pause a rollout", "kubernetes"),
					cmd("resume", "Resume a rollout", "kubernetes"),
					cmd("restart", "Restart a resource", "kubernetes"),
					cmd("undo", "Roll back a rollout", "kubernetes"),
				},
			},
			{
				Name: "scale", Description: "Scale a resource", Icon: "kubernetes",
				Options: []Option{
					optVal("Number of replicas", "--replicas"),
				},
			},
			cmd("edit", "Edit a resource", "kubernetes"),
			cmd("port-forward", "Forward local ports to a pod", "kubernetes"),
			{
				Name: "top", Description: "Show resource usage", Icon: "kubernetes",
				Subcommands: []*Spec{
					cmd("node", "Show node metrics", "kubernetes"),
					cmd("pod", "Show pod metrics", "kubernetes"),
				},
			},
			{
				Name: "config", Description: "Modify kubeconfig files", Icon: "kubernetes",
				Subcommands: []*Spec{
					cmd("get-contexts", "List contexts", "kubernetes"),
					cmd("use-context", "Set the current context", "kubernetes"),
					cmd("set-context", "Set a context entry", "kubernetes"),
					cmd("current-context", "Show the current context", "kubernetes"),
					cmd("view", "Show merged kubeconfig settings", "kubernetes"),
					cmd("set-cluster", "Set a cluster entry", "kubernetes"),
					cmd("set-credentials", "Set a credentials entry", "kubernetes"),
				},
			},
			cmd("create", "Create a resource", "kubernetes"),
			{
				Name: "run", Description: "Run an image in a pod", Icon: "kubernetes",
				Options: []Option{
					optVal("Image to run", "--image"),
					optVal("Restart policy", "--restart"),
				},
			},
			cmd("expose", "Expose a resource as a service", "kubernetes"),
			cmd("annotate", "Update annotations on a resource", "kubernetes"),
			cmd("label", "Update labels on a resource", "kubernetes"),
		},
	}
}

// asdfSpec builds the asdf version manager spec.
func asdfSpec() *Spec {
	return &Spec{
		Name:        "asdf",
		Description: "Extendable version manager",
		Icon:        "package",
		Subcommands: []*Spec{
			cmd("install", "Install a tool version", "package"),
			cmd("uninstall", "Uninstall a tool version", "package"),
			cmd("list", "List installed versions", "package"),
			cmd("current", "Show the current version", "package"),
			cmd("global", "Set the global version", "package"),
			cmd("local", "Set the local version", "package"),
			{
				Name: "plugin", Description: "Manage plugins", Icon: "package",
				Subcommands: []*Spec{
					cmd("add", "Add a plugin", "package"),
					cmd("remove", "Remove a plugin", "package"),
					cmd("list", "List plugins", "package"),
					cmd("update", "Update plugins", "package"),
				},
			},
			cmd("update", "Update asdf itself", "package"),
			cmd("reshim", "Regenerate shims", "package"),
		},
	}
}

// nvmSpec builds the Node Version Manager spec.
func nvmSpec() *Spec {
	return &Spec{
		Name:        "nvm",
		Description: "Node.js version manager",
		Icon:        "node",
		Subcommands: []*Spec{
			cmd("install", "Install a Node.js version", "node"),
			cmd("use", "Switch to a Node.js version", "node"),
			cmd("ls", "List installed versions", "node"),
			cmd("ls-remote", "List available versions", "node"),
			cmd("alias", "Create a version alias", "node"),
			cmd("unalias", "Remove a version alias", "node"),
			cmd("current", "Show the current version", "node"),
			cmd("uninstall", "Remove a Node.js version", "node"),
		},
	}
}

// rbenvSpec builds the rbenv version manager spec.
func rbenvSpec() *Spec {
	return &Spec{
		Name:        "rbenv",
		Description: "Ruby version manager",
		Icon:        "package",
		Subcommands: []*Spec{
			cmd("install", "Install a Ruby version", "package"),
			cmd("uninstall", "Uninstall a Ruby version", "package"),
			cmd("global", "Set the global Ruby version", "package"),
			cmd("local", "Set the local Ruby version", "package"),
			cmd("versions", "List installed Ruby versions", "package"),
			cmd("version", "Show the current Ruby version", "package"),
			cmd("rehash", "Regenerate shims", "package"),
		},
	}
}

// voltaSpec builds the Volta toolchain manager spec.
func voltaSpec() *Spec {
	return &Spec{
		Name:        "volta",
		Description: "JavaScript toolchain manager",
		Icon:        "node",
		Subcommands: []*Spec{
			cmd("install", "Install a tool", "node"),
			cmd("uninstall", "Uninstall a tool", "node"),
			cmd("list", "List installed tools", "node"),
			cmd("pin", "Pin a tool version for the project", "node"),
			cmd("which", "Show the tool binary location", "node"),
		},
	}
}

// rsyncSpec builds the rsync spec. Positional values mix SSH hosts and
// paths; the engine merges file completion with the ssh-hosts generator.
func rsyncSpec() *Spec {
	return &Spec{
		Name:        "rsync",
		Description: "Fast incremental file transfer",
		Icon:        "network",
		Options: []Option{
			optD("Archive mode", "-a", "--archive"),
			optD("Verbose output", "-v", "--verbose"),
			optD("Compress during transfer", "-z", "--compress"),
			optD("Recurse into directories", "-r", "--recursive"),
			optD("Show progress", "-P", "--progress"),
			optVal("Remote shell to use", "-e", "--rsh"),
			optD("Show what would be transferred", "-n", "--dry-run"),
			optD("Delete extraneous files at the destination", "--delete"),
			optD("Skip newer files at the destination", "-u", "--update"),
		},
		Generator: "ssh-hosts",
	}
}

// scpSpec builds the scp spec; the engine merges file completion with the
// ssh-hosts generator for remote paths.
func scpSpec() *Spec {
	return &Spec{
		Name:        "scp",
		Description: "Secure copy over SSH",
		Icon:        "network",
		Options: []Option{
			optVal("SSH port", "-P"),
			optD("Copy recursively", "-r"),
			optGen("files", "SSH identity file", "-i"),
			optD("Verbose output", "-v"),
			optD("Compress during transfer", "-C"),
			optD("Quiet mode", "-q"),
		},
		Generator: "ssh-hosts",
	}
}

// sftpSpec builds the sftp spec.
func sftpSpec() *Spec {
	return &Spec{
		Name:        "sftp",
		Description: "Secure file transfer over SSH",
		Icon:        "network",
		Options: []Option{
			optVal("SSH port", "-P"),
			optGen("files", "SSH identity file", "-i"),
			optD("Recursive copy", "-r"),
		},
		Generator: "ssh-hosts",
	}
}

// sshKeygenSpec builds the ssh-keygen spec.
func sshKeygenSpec() *Spec {
	return &Spec{
		Name:        "ssh-keygen",
		Description: "Generate and manage SSH keys",
		Icon:        "network",
		Options: []Option{
			optVal("Key type (rsa, ed25519, ...)", "-t"),
			optVal("Key size in bits", "-b"),
			optGen("files", "Key file path", "-f"),
			optVal("Key comment", "-C"),
			optD("Print the public key", "-y"),
			optD("Change the passphrase", "-p"),
			optVal("New passphrase", "-N"),
			optD("Quiet mode", "-q"),
		},
	}
}

// sshSpec builds the ssh spec.
func sshSpec() *Spec {
	return &Spec{
		Name:        "ssh",
		Description: "OpenSSH remote login client",
		Icon:        "network",
		Options: []Option{
			optGen("files", "Identity (private key) file", "-i"),
			optVal("Remote port", "-p"),
			optVal("Login user", "-l"),
			optGen("files", "Configuration file", "-F"),
			optVal("Local port forwarding", "-L"),
			optVal("Remote port forwarding", "-R"),
			optVal("Dynamic application-level forwarding", "-D"),
			optD("Do not execute a remote command", "-N"),
			optD("Disable pseudo-terminal allocation", "-T"),
			optD("Verbose output", "-v"),
			optVal("Jump hosts", "-J"),
			optVal("Set an SSH option", "-o"),
			optD("Force pseudo-terminal allocation", "-t"),
			optD("Enable agent forwarding", "-A"),
			optD("Enable X11 forwarding", "-X"),
			optD("Quiet mode", "-q"),
			optD("IPv4 only", "-4"),
			optD("IPv6 only", "-6"),
		},
		Generator: "ssh-hosts",
	}
}

// terraformSpec builds the Terraform/OpenTofu-style spec; terragrunt reuses
// the same subcommand tree.
func terraformSpec(name, desc string) *Spec {
	return &Spec{
		Name:        name,
		Description: desc,
		Icon:        "cloud",
		Subcommands: []*Spec{
			cmd("init", "Initialize the working directory", "cloud"),
			{
				Name: "plan", Description: "Show execution plan", Icon: "cloud",
				Options: []Option{
					optVal("Set an input variable", "-var"),
					optGen("files", "Variable definitions file", "-var-file"),
					optVal("Target a specific resource", "-target"),
				},
			},
			{
				Name: "apply", Description: "Apply the planned changes", Icon: "cloud",
				Options: []Option{
					optD("Skip interactive approval", "-auto-approve"),
					optVal("Set an input variable", "-var"),
					optGen("files", "Variable definitions file", "-var-file"),
					optVal("Target a specific resource", "-target"),
				},
			},
			{
				Name: "destroy", Description: "Destroy managed infrastructure", Icon: "cloud",
				Options: []Option{
					optD("Skip interactive approval", "-auto-approve"),
					optVal("Set an input variable", "-var"),
					optVal("Target a specific resource", "-target"),
				},
			},
			cmd("validate", "Validate the configuration", "cloud"),
			cmd("fmt", "Format configuration files", "cloud"),
			{
				Name: "state", Description: "Manage the state", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list", "List resources in the state", "cloud"),
					cmd("show", "Show a state resource", "cloud"),
					cmd("mv", "Move a state resource", "cloud"),
					cmd("rm", "Remove a state resource", "cloud"),
					cmd("pull", "Download the current state", "cloud"),
				},
			},
			{
				Name: "workspace", Description: "Manage workspaces", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("list", "List workspaces", "cloud"),
					cmd("new", "Create a workspace", "cloud"),
					cmd("select", "Select a workspace", "cloud"),
					cmd("delete", "Delete a workspace", "cloud"),
					cmd("show", "Show the current workspace", "cloud"),
				},
			},
			cmd("output", "Show output values", "cloud"),
			{
				Name: "providers", Description: "Show provider information", Icon: "cloud",
				Subcommands: []*Spec{
					cmd("lock", "Write provider dependency locks", "cloud"),
					cmd("mirror", "Mirror providers to a local directory", "cloud"),
				},
			},
		},
	}
}
