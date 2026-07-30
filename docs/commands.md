# Built-in command catalog

Generated from the checked-in IRIS snapshot used by argmax at runtime.

| Category | Count |
| --- | ---: |
| Cloud, containers, Kubernetes, DevOps, and databases | 118 |
| JavaScript, TypeScript, frontend, and Node.js tools | 82 |
| Python ecosystem and data science | 19 |
| Rust ecosystem and modern CLI tools | 11 |
| Go development and project tools | 3 |
| Java, Kotlin, and JVM build tools | 14 |
| C/C++ compilers and build systems | 16 |
| Git version control and GitHub tools | 8 |
| System package managers | 12 |
| Filesystem, directory, and archive utilities | 30 |
| Editors, pagers, and file viewers | 27 |
| Text processing, JSON, and stream manipulation | 28 |
| Task runners and build automation | 24 |
| System administration, network, and process management | 175 |
| **Total** | **567** |

The 567 documented records resolve to **566 unique command roots**; the duplicate `find` records are explicitly merged.

## Cloud, containers, Kubernetes, DevOps, and databases

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `amplify` | Environment | `ops/amplify.go` | migrated | 18 | 8 | 0 |
| `ampx` | CLI for Amplify Gen 2 | `ops/ampx.go` | migrated | 13 | 27 | 0 |
| `ansible` | Define and run a single Ansible task | `ops/ansible.go` | migrated | 0 | 34 | 0 |
| `ansible-config` | View ansible configuration | `ops/ansible_config.go` | migrated | 8 | 9 | 0 |
| `ansible-doc` | Displays information on modules installed in Ansible libraries | `ops/ansible_doc.go` | migrated | 0 | 11 | 0 |
| `ansible-galaxy` | The Galaxy API server URL | `ops/ansible_galaxy.go` | migrated | 20 | 30 | 0 |
| `ansible-lint` | Ansible static code analysis | `ops/ansible_lint.go` | migrated | 0 | 20 | 0 |
| `ansible-playbook` | Runs Ansible playbooks, executing the defined tasks on the targeted hosts | `ops/ansible_playbook.go` | migrated | 0 | 36 | 0 |
| `appwrite` | Appwrite - Open-Source End-to-End Backend Server | `ops/appwrite.go` | migrated | 38 | 40 | 0 |
| `arch` | 32-bit intel | `ops/arch.go` | migrated | 0 | 7 | 0 |
| `arduino-cli` | Arduino CLI - build, compile, and upload Arduino sketches | `ops/arduino_cli.go` | migrated | 43 | 40 | 0 |
| `argo` | If True, Use the HTTP client. Defaults to the ARGO_HTTP1 environment variable | `ops/argo.go` | migrated | 31 | 40 | 0 |
| `asdf` | Plugin name | `ops/asdf.go` | migrated | 0 | 6 | 0 |
| `atlas` | CLI tool to manage MongoDB Atlas | `ops/atlas.go` | migrated | 50 | 40 | 0 |
| `aws` | Use a specific profile from your credential file | `ops/aws.go` | migrated | 50 | 1 | 0 |
| `aws-vault` | Add credentials to the secure keystore | `ops/aws_vault.go` | migrated | 0 | 11 | 0 |
| `bit` | Bit documentation: https://bit.dev/docs | `ops/bit.go` | migrated | 50 | 40 | 0 |
| `bosh` | Deployment | `ops/bosh.go` | migrated | 50 | 14 | 0 |
| `capacitor` | Add a native platform project to your app | `ops/capacitor.go` | migrated | 4 | 6 | 0 |
| `cdk` | AWS CDK CLI | `ops/cdk.go` | migrated | 15 | 2 | 0 |
| `cf` | Cloudfoundry cli | `ops/cf.go` | migrated | 49 | 36 | 0 |
| `checkov` | Branch | `ops/checkov.go` | migrated | 0 | 32 | 0 |
| `circleci` | CircleCI CLI | `ops/circleci.go` | migrated | 46 | 19 | 0 |
| `cloudflared` | Specify the hostname of your application | `ops/cloudflared.go` | migrated | 3 | 40 | 0 |
| `coda` | Coda CLI - interact with Coda docs and tables | `ops/coda.go` | migrated | 15 | 5 | 0 |
| `command` | Run an external command | `ops/command.go` | migrated | 0 | 1 | 0 |
| `copilot` | Name of the application | `ops/copilot.go` | migrated | 22 | 40 | 0 |
| `cosign` | Provides utilities for attaching artifacts to other artifacts in a registry | `ops/cosign.go` | migrated | 28 | 40 | 0 |
| `dapr` | Distributed Application Runtime CLI | `ops/dapr.go` | migrated | 22 | 40 | 0 |
| `datree` | Help for | `ops/datree.go` | migrated | 8 | 10 | 0 |
| `deployctl` | Command line tool for Deno Deploy | `ops/deployctl.go` | migrated | 2 | 8 | 0 |
| `direnv` | Help for direnv | `ops/direnv.go` | migrated | 18 | 0 | 0 |
| `docker` | container engine | `ops/docker.go` | migrated | 43 | 44 | 13 |
| `docker-compose` | multi-container (legacy) | `ops/docker.go` | migrated | 8 | 5 | 0 |
| `doctl` | The official DigitalOcean command line interface (CLI) | `ops/doctl.go` | migrated | 50 | 6 | 0 |
| `doppler` | The official CLI for Doppler Secret Operations Platform | `ops/doppler.go` | migrated | 43 | 40 | 0 |
| `eas` | Log in with your Expo account | `ops/eas.go` | migrated | 49 | 40 | 0 |
| `fastly` | A CLI for interacting with the Fastly platform | `ops/fastly.go` | migrated | 50 | 40 | 0 |
| `firebase` | ProjectAlias | `ops/firebase.go` | migrated | 50 | 40 | 0 |
| `flyctl` | Command line tool for Fly.io services | `ops/flyctl.go` | migrated | 50 | 40 | 0 |
| `fnm` | Fast and simple Node.js version manager | `ops/fnm.go` | migrated | 14 | 10 | 0 |
| `gcloud` | Manage Google Cloud Platform resources and developer workflow | `ops/gcloud.go` | migrated | 50 | 0 | 0 |
| `gh` | Current branch | `ops/gh.go` | migrated | 50 | 40 | 0 |
| `gpg` | Encryption and signing tool | `ops/gpg.go` | migrated | 0 | 40 | 0 |
| `hasura` | .env filename to load ENV vars from | `ops/hasura.go` | migrated | 31 | 40 | 0 |
| `helm` | The Helm package manager for Kubernetes | `ops/helm.go` | migrated | 48 | 40 | 0 |
| `helmfile` | Deploy helm charts | `ops/helmfile.go` | migrated | 15 | 11 | 0 |
| `hugo` | The world | `ops/hugo.go` | migrated | 38 | 40 | 0 |
| `k3d` | Little helper to run k3s in Docker | `ops/k3d.go` | migrated | 37 | 40 | 0 |
| `k6` | Create an archive | `ops/k6.go` | migrated | 50 | 40 | 0 |
| `k9s` | Kubernetes namespace | `ops/k9s.go` | migrated | 3 | 24 | 0 |
| `kind` | Cluster | `ops/kind.go` | migrated | 23 | 16 | 0 |
| `knex` | SQL query builder for JavaScript | `ops/knex.go` | migrated | 12 | 14 | 0 |
| `kubectl` | kubernetes cli | `ops/kubectl.go` | migrated | 13 | 4 | 0 |
| `kubectx` | Switch between Kubernetes-contexts | `ops/kubectx.go` | migrated | 0 | 4 | 0 |
| `kubens` | Switch between Kubernetes-namespaces | `ops/kubens.go` | migrated | 0 | 2 | 0 |
| `limactl` | Lima: Linux virtual machines, with a focus on running containers | `ops/limactl.go` | migrated | 12 | 10 | 0 |
| `locust` | Show program | `ops/locust.go` | migrated | 0 | 26 | 0 |
| `lpass` | Command line interface for LastPass | `ops/lpass.go` | migrated | 37 | 36 | 0 |
| `minikube` | Format to print stdout in | `ops/minikube.go` | migrated | 50 | 40 | 0 |
| `mongocli` | CLI tool to manage your MongoDB Cloud | `ops/mongocli.go` | migrated | 50 | 40 | 0 |
| `mongoimport` | Import data from a JSON, CSV, or TSV file into a MongoDB instance | `ops/mongoimport.go` | migrated | 0 | 27 | 0 |
| `mongosh` | Default Connection String; Equivalent to running mongosh without any commands | `ops/mongosh.go` | migrated | 0 | 11 | 0 |
| `multipass` | Displays help on commandline options | `ops/multipass.go` | migrated | 31 | 19 | 0 |
| `mysql` | Mysql is a terminal-based front-end to MySQL | `ops/mysql.go` | migrated | 0 | 40 | 0 |
| `netlify` | Print debugging information | `ops/netlify.go` | migrated | 2 | 39 | 0 |
| `newman` | Newman is a command-line collection runner for Postman | `ops/newman.go` | migrated | 2 | 31 | 0 |
| `nginx` | Nginx (pronounced | `ops/nginx.go` | migrated | 0 | 9 | 0 |
| `ngrok` | Path to log file, | `ops/ngrok.go` | migrated | 10 | 5 | 0 |
| `nvm` | Node version | `ops/nvm.go` | migrated | 14 | 11 | 0 |
| `oci` | Oracle Cloud Infrastructure CLI | `ops/oci.go` | migrated | 25 | 29 | 0 |
| `okteto` | Context | `ops/okteto.go` | migrated | 33 | 36 | 0 |
| `op` | Official 1Password CLI | `ops/op.go` | migrated | 32 | 40 | 0 |
| `opa` | Open Policy Agent (OPA) | `ops/opa.go` | migrated | 19 | 40 | 0 |
| `osqueryi` | Your OS as a high-performance relational database | `ops/osqueryi.go` | migrated | 0 | 40 | 0 |
| `pass` | Pass - stores, retrieves, generates, and synchronizes passwords securely | `ops/pass.go` | migrated | 17 | 9 | 0 |
| `pg_dump` | Dumps a database as a text file or to other formats | `ops/pg_dump.go` | migrated | 0 | 40 | 0 |
| `pgcli` | Host address of the postgres database | `ops/pgcli.go` | migrated | 0 | 18 | 0 |
| `pm2` | Outputs the version number | `ops/pm2.go` | migrated | 50 | 36 | 0 |
| `pod` | CocoaPods, the Cocoa library package manager | `ops/pod.go` | migrated | 1 | 40 | 0 |
| `podman` | Build an image using instructions from Containerfiles | `ops/podman.go` | migrated | 36 | 40 | 0 |
| `pscale` | The client ID for the PlanetScale CLI application | `ops/pscale.go` | migrated | 35 | 28 | 0 |
| `psql` | Psql is a terminal-based front-end to PostgreSQL | `ops/psql.go` | migrated | 0 | 3 | 0 |
| `pulumi` | The name of the stack to operate on. Defaults to the current stack | `ops/pulumi.go` | migrated | 49 | 40 | 0 |
| `qodana` | Run Qodana as fast as possible, with minimum effort required | `ops/qodana.go` | migrated | 4 | 16 | 0 |
| `railway` | CLI for managing Railway Apps | `ops/railway.go` | migrated | 34 | 8 | 0 |
| `rbenv` | List all available rbenv commands | `ops/rbenv.go` | migrated | 10 | 4 | 0 |
| `robot` | Tag | `ops/robot.go` | migrated | 0 | 40 | 0 |
| `rsync` | remote sync | `ops/ssh.go` | migrated | 0 | 7 | 1 |
| `scp` | secure copy | `ops/ssh.go` | migrated | 0 | 4 | 1 |
| `serverless` | AWS profile to use with the command | `ops/serverless.go` | migrated | 20 | 24 | 0 |
| `sfdx` | Analyze (lint) Aura component code | `ops/sfdx.go` | migrated | 50 | 40 | 0 |
| `sftp` | OpenSSH secure file transfer | `ops/sftp.go` | migrated | 0 | 17 | 0 |
| `space` | Deta Space CLI for mananging Deta Space projects | `ops/space.go` | migrated | 15 | 14 | 0 |
| `sqlite3` | A command line interface for SQLite version 3 | `ops/sqlite3.go` | migrated | 0 | 28 | 0 |
| `src` | Interact with Sourcegraph from the command line | `ops/src.go` | migrated | 14 | 15 | 0 |
| `ssh` | secure shell | `ops/ssh.go` | migrated | 0 | 10 | 1 |
| `ssh-keygen` | Generates, manages and converts authentication keys for ssh | `ops/ssh_keygen.go` | migrated | 0 | 39 | 0 |
| `stripe` | Stripe CLI - build, test, and manage your Stripe integrations right from your terminal | `ops/stripe.go` | migrated | 46 | 36 | 0 |
| `supabase` | Supabase CLI | `ops/supabase.go` | migrated | 43 | 27 | 0 |
| `surreal` | Database authentication password to use when connecting [default: root] | `ops/surreal.go` | migrated | 17 | 12 | 0 |
| `tailscale` | Connect to Tailscale, logging in if needed | `ops/tailscale.go` | migrated | 17 | 37 | 0 |
| `terraform` | Workspace | `ops/terraform.go` | migrated | 4 | 19 | 0 |
| `terragrunt` | Workspace | `ops/terragrunt.go` | migrated | 4 | 30 | 0 |
| `tfenv` | Version | `ops/tfenv.go` | migrated | 8 | 2 | 0 |
| `tfsec` | Terraform workspaces | `ops/tfsec.go` | migrated | 0 | 27 | 0 |
| `tkn` | CLI for tekton pipelines | `ops/tkn.go` | migrated | 40 | 40 | 0 |
| `trivy` | Skip updating built-in policies [$TRIVY_SKIP_POLICY_UPDATE] | `ops/trivy.go` | migrated | 17 | 29 | 0 |
| `tsuru` | Plan | `ops/tsuru.go` | migrated | 43 | 25 | 0 |
| `vault` | Display help | `ops/vault.go` | migrated | 28 | 40 | 0 |
| `vela` | Show the reference doc for component, trait or workflow types | `ops/vela.go` | migrated | 46 | 31 | 0 |
| `vercel` | CLI Interface for Vercel.com | `ops/vercel.go` | migrated | 39 | 31 | 0 |
| `volta` | Enables verbose diagnostics | `ops/volta.go` | migrated | 11 | 15 | 0 |
| `watson` | A wonderful CLI to track your time | `ops/watson.go` | migrated | 20 | 31 | 0 |
| `whois` | Query a database for information about a domain registrant | `ops/whois.go` | migrated | 0 | 17 | 0 |
| `wrangler` | Path to configuration file [default: wrangler.toml] | `ops/wrangler.go` | migrated | 24 | 18 | 0 |
| `xc` | List tasks from an xc-compatible markdown file | `ops/xc.go` | migrated | 0 | 5 | 0 |
| `xcodes` | Manage the Xcode versions installed on your Mac | `ops/xcodes.go` | migrated | 10 | 11 | 0 |

## JavaScript, TypeScript, frontend, and Node.js tools

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `asar` | A simple extensive tar-like archive format with indexing | `js/asar.go` | migrated | 8 | 2 | 0 |
| `astro` | Add an integration | `js/astro.go` | migrated | 11 | 14 | 0 |
| `babel` | A comma-separated list of preset names | `js/babel.go` | migrated | 0 | 32 | 0 |
| `blitz` | Show help for command | `js/blitz.go` | migrated | 18 | 15 | 0 |
| `browser-sync` | Keep multiple browsers & devices in sync when building websites | `js/browser_sync.go` | migrated | 5 | 40 | 0 |
| `build-storybook` | Storybook build CLI tools | `js/build_storybook.go` | migrated | 0 | 3 | 0 |
| `bun` | bun js runtime | `js/bun.go` | migrated | 23 | 57 | 2 |
| `bunx` | execute package (bun x) | `js/bun.go` | migrated | 0 | 2 | 0 |
| `cordova` | Print out the version of your cordova-cli install | `js/cordova.go` | migrated | 25 | 17 | 0 |
| `create-completion-spec` | Setup fig folder and create spec with the given name | `js/create_completion_spec.go` | migrated | 1 | 2 | 0 |
| `create-next-app` | Output the version number | `js/create_next_app.go` | migrated | 0 | 5 | 0 |
| `create-nx-workspace` | The name of the workspace | `js/create_nx_workspace.go` | migrated | 0 | 8 | 0 |
| `create-react-app` | Creates a new React project | `js/create_react_app.go` | migrated | 0 | 5 | 0 |
| `create-react-native-app` | Creates a new React Native project | `js/create_react_native_app.go` | migrated | 0 | 6 | 0 |
| `create-redwood-app` | Name of your Redwood project | `js/create_redwood_app.go` | migrated | 0 | 9 | 0 |
| `create-remix` | Display help for command | `js/create_remix.go` | migrated | 0 | 2 | 0 |
| `create-t3-app` | The name of the application, as well as the name of the directory to create | `js/create_t3_app.go` | migrated | 0 | 8 | 0 |
| `create-video` | CLI used to create remotion video project | `js/create_video.go` | migrated | 0 | 0 | 0 |
| `create-vite` | Create a new project powered by Vite | `js/create_vite.go` | migrated | 0 | 0 | 0 |
| `create-web3-frontend` | Quickly create a Next.js project with wagmi and TailwindCSS ready to go | `js/create_web3_frontend.go` | migrated | 0 | 4 | 0 |
| `deno` | A modern JavaScript and TypeScript runtime | `js/deno.go` | migrated | 24 | 40 | 0 |
| `dotenv` | Loads environment variables from .env | `js/dotenv.go` | migrated | 0 | 4 | 0 |
| `electron` | Build cross platform desktop apps with JavaScript, HTML and CSS | `js/electron.go` | migrated | 0 | 4 | 0 |
| `elm` | Fig spec for the Elm language cli | `js/elm.go` | migrated | 6 | 8 | 0 |
| `elm-format` | Format your code in the Elm idiomatic way | `js/elm_format.go` | migrated | 0 | 6 | 0 |
| `elm-json` | Deal with your elm.json | `js/elm_json.go` | migrated | 8 | 7 | 0 |
| `elm-review` | Prints a single JSON object | `js/elm_review.go` | migrated | 4 | 20 | 0 |
| `esbuild` | An extremely fast JavaScript bundler | `js/esbuild.go` | migrated | 0 | 40 | 0 |
| `eslint` | Pluggable JavaScript linter | `js/eslint.go` | migrated | 0 | 35 | 0 |
| `expo` | Tools for creating, running, and deploying Universal Expo and React Native apps | `js/expo.go` | migrated | 0 | 40 | 0 |
| `expo-cli` | Tools for creating, running, and deploying Universal Expo and React Native apps | `js/expo_cli.go` | migrated | 0 | 40 | 0 |
| `ganache-cli` | Fast Ethereum RPC client | `js/ganache_cli.go` | migrated | 0 | 20 | 0 |
| `gatsby` | Set host. Defaults to localhost | `js/gatsby.go` | migrated | 8 | 13 | 0 |
| `hardhat` | Ethereum development environment | `js/hardhat.go` | migrated | 10 | 14 | 0 |
| `ionic` | Target engine (e.g. browser, cordova) | `js/ionic.go` | migrated | 35 | 40 | 0 |
| `jest` | A delightful JavaScript Testing Framework with a focus on simplicity | `js/jest.go` | migrated | 0 | 40 | 0 |
| `lerna` | Branch | `js/lerna.go` | migrated | 10 | 40 | 0 |
| `meteor` | Run the meteor command-line tool | `js/meteor.go` | migrated | 22 | 40 | 0 |
| `ncu` | Clear the default cache, or the cache file specified by --cacheFile | `js/ncu.go` | migrated | 0 | 25 | 0 |
| `nest` | Report actions that would be taken without writing out results | `js/nest.go` | migrated | 27 | 7 | 0 |
| `next` | A port number on which to start the application | `js/next.go` | migrated | 8 | 7 | 0 |
| `ng` | Project name | `js/ng.go` | migrated | 8 | 7 | 0 |
| `node` | Run the node interpreter | `js/node.go` | migrated | 0 | 31 | 0 |
| `npm` | node packages | `js/npm.go` | migrated | 18 | 10 | 1 |
| `npx` | Execute binaries from npm packages | `js/npx.go` | migrated | 0 | 8 | 0 |
| `nuxi` | The directory of the target application | `js/nuxi.go` | migrated | 0 | 19 | 0 |
| `nuxt` | Launch the development server | `js/nuxt.go` | migrated | 3 | 3 | 0 |
| `nx` | All projects | `js/nx.go` | migrated | 21 | 37 | 0 |
| `oxlint` | All lints (except nursery) | `js/oxlint.go` | migrated | 0 | 16 | 0 |
| `playwright` | Display help for command | `js/playwright.go` | migrated | 4 | 5 | 0 |
| `pnpm` | fast node packages | `js/pnpm.go` | migrated | 19 | 11 | 1 |
| `pnpx` | Execute binaries from npm packages | `js/pnpx.go` | migrated | 0 | 7 | 0 |
| `prettier` | Run Prettier from the command line | `js/prettier.go` | migrated | 0 | 39 | 0 |
| `quasar` | Quasar Framework CLI | `js/quasar.go` | migrated | 5 | 23 | 0 |
| `react-native` | Attempt to fix all diagnosed issues | `js/react_native.go` | migrated | 0 | 40 | 0 |
| `redwood` | Script | `js/redwood.go` | migrated | 35 | 24 | 0 |
| `remix` | Represent the directory of the Remix application | `js/remix.go` | migrated | 4 | 4 | 0 |
| `remotion` | Create videos programmatically in React | `js/remotion.go` | migrated | 40 | 40 | 0 |
| `rollup` | Next-generation ES module bundler | `js/rollup.go` | migrated | 0 | 40 | 0 |
| `rome` | Rome CLI | `js/rome.go` | migrated | 10 | 22 | 0 |
| `rush` | Projects | `js/rush.go` | migrated | 11 | 26 | 0 |
| `sequelize` | The environment to run the command in | `js/sequelize.go` | migrated | 0 | 22 | 0 |
| `serve` | Static file serving and directory listing | `js/serve.go` | migrated | 0 | 16 | 0 |
| `shadcn-ui` | Shadcn UI CLI | `js/shadcn_ui.go` | migrated | 5 | 4 | 0 |
| `start-storybook` | Display usage information | `js/start_storybook.go` | migrated | 0 | 16 | 0 |
| `stencil` | CLI to build Stencil projects and generate components | `js/stencil.go` | migrated | 7 | 22 | 0 |
| `swagger-typescript-api` | Generate api via swagger scheme | `js/swagger_typescript_api.go` | migrated | 0 | 33 | 0 |
| `swc` | Path to the file | `js/swc.go` | migrated | 0 | 19 | 0 |
| `truffle` | Execute build pipeline (if configuration present) | `js/truffle.go` | migrated | 31 | 29 | 0 |
| `ts-node` | Run the TypeScript interpreter for Node.JS | `js/ts_node.go` | migrated | 0 | 26 | 0 |
| `tsc` | CLI tool for TypeScript compiler | `js/tsc.go` | migrated | 0 | 40 | 0 |
| `tsx` | Run TypeScript file using tsx | `js/tsx.go` | migrated | 1 | 5 | 0 |
| `turbo` | Print the version | `js/turbo.go` | migrated | 10 | 22 | 0 |
| `typeorm` | Show help for command | `js/typeorm.go` | migrated | 14 | 15 | 0 |
| `vite` | Native ESM-powered web dev build tool | `js/vite.go` | migrated | 0 | 27 | 0 |
| `vr` | The npm-style script runner for Deno | `js/vr.go` | migrated | 3 | 3 | 0 |
| `vsce` | The Visual Studio Code Extension Manager | `js/vsce.go` | migrated | 12 | 22 | 0 |
| `vue` | Vue cli tools | `js/vue.go` | migrated | 13 | 33 | 0 |
| `watchman` | A file watching service | `js/watchman.go` | migrated | 22 | 20 | 0 |
| `webpack` | Run webpack (default command, can be omitted) | `js/webpack.go` | migrated | 10 | 40 | 0 |
| `yalc` | Work with yarn/npm packages locally like a boss | `js/yalc.go` | migrated | 12 | 14 | 0 |
| `yarn` | yarn package manager | `js/yarn.go` | migrated | 22 | 12 | 1 |

## Python ecosystem and data science

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `black` | Version | `python/black.go` | migrated | 0 | 24 | 0 |
| `conda` | Name of environment | `python/conda.go` | migrated | 21 | 40 | 0 |
| `django-admin` | Show this help message and exit | `python/django_admin.go` | migrated | 1 | 40 | 0 |
| `googler` | Google from the command-line | `python/googler.go` | migrated | 0 | 29 | 0 |
| `jupyter` | Set log level to logging.DEBUG (maximize logging output) | `python/jupyter.go` | migrated | 17 | 17 | 0 |
| `mamba` | Mamba is a fast, robust, and cross-platform package manager | `python/mamba.go` | migrated | 26 | 40 | 0 |
| `mypy` | Mypy is a static type checker for Python | `python/mypy.go` | migrated | 0 | 36 | 0 |
| `pipenv` | Python package manager | `python/pipenv.go` | migrated | 12 | 40 | 0 |
| `pipx` | Installed package | `python/pipx.go` | migrated | 14 | 19 | 0 |
| `poetry` | python dependency manager | `python/python.go` | migrated | 17 | 2 | 2 |
| `pre-commit` | Show help message and exit | `python/pre_commit.go` | migrated | 13 | 22 | 0 |
| `pyenv` | Pyenv | `python/pyenv.go` | migrated | 8 | 11 | 0 |
| `pytest` | Control assertion debugging tools. | `python/pytest.go` | migrated | 0 | 40 | 0 |
| `ruff` | Enable verbose logging | `python/ruff.go` | migrated | 1 | 36 | 0 |
| `sqlfluff` | A dialect-flexible and configurable SQL linter | `python/sqlfluff.go` | migrated | 10 | 22 | 0 |
| `sqlmesh` | SQLMesh command line tool | `python/sqlmesh.go` | migrated | 16 | 40 | 0 |
| `streamlit` | Streamlit | `python/streamlit.go` | migrated | 11 | 3 | 0 |
| `uv` | fast python package manager | `python/python.go` | migrated | 27 | 8 | 5 |
| `youtube-dl` | Clipboard | `python/youtube_dl.go` | migrated | 0 | 40 | 0 |

## Rust ecosystem and modern CLI tools

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `cargo` | rust toolchain | `rust/cargo.go` | migrated | 13 | 21 | 10 |
| `dprint` | Prints the help of the given subcommand(s) | `rust/dprint.go` | migrated | 9 | 6 | 0 |
| `pijul` | Adds a path to the tree | `rust/pijul.go` | migrated | 32 | 29 | 0 |
| `rustc` | Rust compiler | `rust/rustc.go` | migrated | 0 | 17 | 1 |
| `rustup` | The Rust toolchain installer | `rust/rustup.go` | migrated | 27 | 27 | 0 |
| `taplo` | Set color values for the output | `rust/taplo.go` | migrated | 11 | 19 | 0 |
| `tokei` | Count your code, quickly | `rust/tokei.go` | migrated | 0 | 13 | 0 |
| `trunk` | Run on all files instead of only changed files | `rust/trunk.go` | migrated | 18 | 21 | 0 |
| `wasm-bindgen` | Generate bindings between WebAssembly and JavaScript | `rust/wasm_bindgen.go` | migrated | 0 | 22 | 0 |
| `wasm-pack` | Build an npm package | `rust/wasm_pack.go` | migrated | 2 | 5 | 0 |
| `zellij` | Change where zellij looks for the configuration file | `rust/zellij.go` | migrated | 0 | 40 | 0 |

## Go development and project tools

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `go` | tool for managing Go source code | `golang/go.go` | migrated | 42 | 158 | 5 |
| `goctl` | A cli tool to generate go-zero code | `golang/goctl.go` | migrated | 40 | 40 | 0 |
| `goreleaser` | Deliver Go binaries as fast and easily as possible | `golang/goreleaser.go` | migrated | 12 | 29 | 0 |

## Java, Kotlin, and JVM build tools

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `clojure` | An alias to refer to its function or a qualified function | `jvm/clojure.go` | migrated | 0 | 23 | 0 |
| `dart` | The Dart file containing the main function | `jvm/dart.go` | migrated | 50 | 40 | 0 |
| `flutter` | Available emulators | `jvm/flutter.go` | migrated | 44 | 40 | 0 |
| `fvm` | Print this usage information | `jvm/fvm.go` | migrated | 18 | 10 | 0 |
| `gradle` | Log all warnings | `jvm/gradle.go` | migrated | 11 | 31 | 0 |
| `java` | Java runtime | `jvm/jvm.go` | migrated | 0 | 9 | 1 |
| `javac` | Java compiler | `jvm/jvm.go` | migrated | 0 | 11 | 1 |
| `jenv` | Executable file | `jvm/jenv.go` | migrated | 29 | 7 | 0 |
| `jmeter` | Apache JMeter - 100% Java Load Testing Tool | `jvm/jmeter.go` | migrated | 0 | 27 | 0 |
| `kdoctor` | Report a version of KDoctor | `jvm/kdoctor.go` | migrated | 0 | 5 | 0 |
| `keytool` | Show help message | `jvm/keytool.go` | migrated | 0 | 40 | 0 |
| `kotlinc` | Kotlin compiler | `jvm/jvm.go` | migrated | 0 | 5 | 1 |
| `mvn` | Maven - a Java based project management and comprehension tool | `jvm/mvn.go` | migrated | 9 | 39 | 1 |
| `spring` | Initialize a new project using Spring Initializr | `jvm/spring.go` | migrated | 9 | 19 | 0 |

## C/C++ compilers and build systems

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `bazel` | Bazel target | `cc/bazel.go` | migrated | 3 | 29 | 0 |
| `c++` | C++ compiler (alias) | `cc/cc.go` | migrated | 0 | 27 | 1 |
| `cc` | C compiler (alias) | `cc/cc.go` | migrated | 0 | 27 | 1 |
| `clang` | LLVM C compiler | `cc/cc.go` | migrated | 0 | 27 | 1 |
| `clang++` | LLVM C++ compiler | `cc/cc.go` | migrated | 0 | 27 | 1 |
| `cmake` | Command-line interface of the cross-platform buildsystem generator CMake | `cc/cmake.go` | migrated | 26 | 46 | 6 |
| `g++` | GNU C++ compiler | `cc/cc.go` | migrated | 0 | 27 | 1 |
| `gcc` | GNU C compiler | `cc/cc.go` | migrated | 0 | 27 | 1 |
| `premake` | The premake5.lua file | `cc/premake.go` | migrated | 14 | 14 | 0 |
| `swift` | Show help information | `cc/swift.go` | migrated | 48 | 40 | 0 |
| `typst` | The Typst compiler | `cc/typst.go` | migrated | 10 | 16 | 0 |
| `xcode-select` | Active developer directory for Xcode tools | `cc/xcode_select.go` | migrated | 0 | 6 | 0 |
| `xcodebuild` | Build Xcode projects | `cc/xcodebuild.go` | migrated | 0 | 40 | 0 |
| `xcodeproj` | Xcodeproj lets you create and modify Xcode projects | `cc/xcodeproj.go` | migrated | 0 | 8 | 0 |
| `xcrun` | SceneKit CLI utilities | `cc/xcrun.go` | migrated | 1 | 21 | 0 |
| `zig` | Enable or disable colored message | `cc/zig.go` | migrated | 30 | 40 | 0 |

## Git version control and GitHub tools

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `ghq` | Clone/sync with a remote repository | `git/ghq.go` | migrated | 4 | 15 | 0 |
| `git` | version control | `git/git.go` | migrated | 54 | 67 | 33 |
| `git-cliff` | Increases the logging verbosity | `git/git_cliff.go` | migrated | 0 | 21 | 0 |
| `git-flow` | Git extensions to provide high-level repository operations for Vincent Driessen | `git/git_flow.go` | migrated | 11 | 2 | 0 |
| `git-profile` | Use profile | `git/git_profile.go` | migrated | 2 | 1 | 0 |
| `git-quick-stats` | Show help for git-quick-stats | `git/git_quick_stats.go` | migrated | 0 | 19 | 0 |
| `github` | Open a git repository in GitHub Desktop | `git/github.go` | migrated | 3 | 2 | 0 |
| `svn` | Specify a username ARG | `git/svn.go` | migrated | 7 | 12 | 0 |

## System package managers

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `apt` | Debian/Ubuntu package manager | `pkginstaller/pkgmgr.go` | migrated | 16 | 8 | 4 |
| `apt-get` | Debian/Ubuntu package manager (low-level) | `pkginstaller/pkgmgr.go` | migrated | 11 | 5 | 2 |
| `brew` | Homebrew package manager | `pkginstaller/pkgmgr.go` | migrated | 26 | 5 | 6 |
| `dnf` | Fedora/RHEL package manager | `pkginstaller/pkgmgr.go` | migrated | 15 | 5 | 1 |
| `dpkg` | Debian package management system | `pkginstaller/dpkg.go` | migrated | 10 | 37 | 0 |
| `flatpak` | flatpak package manager | `pkginstaller/pkgmgr.go` | migrated | 11 | 3 | 0 |
| `pacman` | Arch package manager | `pkginstaller/pkgmgr.go` | migrated | 0 | 21 | 1 |
| `paru` | AUR helper (feature-rich) | `pkginstaller/pkgmgr.go` | migrated | 0 | 10 | 1 |
| `pkgutil` | Query and manipulate for macOS Installer packages and receipts | `pkginstaller/pkgutil.go` | migrated | 5 | 28 | 0 |
| `snap` | snap package manager | `pkginstaller/pkgmgr.go` | migrated | 12 | 4 | 0 |
| `yay` | AUR helper (pacman wrapper) | `pkginstaller/pkgmgr.go` | migrated | 0 | 17 | 1 |
| `yum` | RHEL/CentOS package manager (legacy) | `pkginstaller/pkgmgr.go` | migrated | 8 | 2 | 1 |

## Filesystem, directory, and archive utilities

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `broot` | Show the last modified date of files and directories | `fs/broot.go` | migrated | 0 | 30 | 0 |
| `cd` | change directory | `fs/cd.go` | migrated | 0 | 0 | 1 |
| `chmod` | change file permissions | `fs/chmod.go` | migrated | 20 | 14 | 21 |
| `chown` | change file owner | `fs/chown.go` | migrated | 0 | 1 | 1 |
| `cp` | copy files and directories | `fs/cp.go` | migrated | 0 | 6 | 1 |
| `df` | Display free disk space | `fs/df.go` | migrated | 0 | 8 | 0 |
| `dust` | Like du but more intuitive | `fs/dust.go` | migrated | 0 | 17 | 0 |
| `exa` | A modern replacement for ls | `fs/exa.go` | migrated | 0 | 0 | 0 |
| `eza` | A modern replacement for ls | `fs/eza.go` | migrated | 0 | 40 | 0 |
| `find` | Walk a file hierarchy | `fs/find.go` | merged into `find` | 0 | 0 | 0 |
| `fold` | Fold long lines for finite width output device | `fs/fold.go` | migrated | 0 | 1 | 0 |
| `ln` | create links | `fs/ln.go` | migrated | 0 | 2 | 1 |
| `ls` | list directory contents | `fs/ls.go` | migrated | 0 | 13 | 1 |
| `lsd` | An ls command with a lot of pretty colors and some other stuff | `fs/lsd.go` | migrated | 0 | 29 | 0 |
| `mkdir` | make directories | `fs/mkdir.go` | migrated | 0 | 2 | 0 |
| `mv` | move (rename) files | `fs/mv.go` | migrated | 0 | 4 | 1 |
| `paper` | The Paper CLI | `fs/paper.go` | migrated | 19 | 40 | 0 |
| `rclone` | Only list directories | `fs/rclone.go` | migrated | 50 | 40 | 0 |
| `readlink` | Display file status | `fs/readlink.go` | migrated | 0 | 1 | 0 |
| `rm` | remove files or directories | `fs/rm.go` | migrated | 0 | 5 | 1 |
| `rmdir` | Remove directories | `fs/rmdir.go` | migrated | 0 | 1 | 0 |
| `stow` | Manage farms of symbolic links | `fs/stow.go` | migrated | 0 | 4 | 0 |
| `tar` | Use archive file or device ARCHIVE | `fs/tar.go` | migrated | 31 | 51 | 3 |
| `touch` | create or update file timestamp | `fs/touch.go` | migrated | 0 | 0 | 1 |
| `trash` | Trash, move files/folders to the trash | `fs/trash.go` | migrated | 0 | 5 | 0 |
| `tree` | Display directories as trees (with optional color/HTML output) | `fs/tree.go` | migrated | 0 | 40 | 0 |
| `unzip` | Extract compressed files in a ZIP archive | `fs/unzip.go` | migrated | 0 | 1 | 0 |
| `z` | jump to directory | `fs/zoxide.go` | migrated | 0 | 0 | 2 |
| `zi` | jump to directory interactively | `fs/zoxide.go` | migrated | 0 | 0 | 2 |
| `zip` | Package and compress (archive) files into zip file | `fs/zip.go` | migrated | 0 | 2 | 0 |

## Editors, pagers, and file viewers

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `bat` | A cat(1) clone with syntax highlighting and Git integration | `view/bat.go` | migrated | 0 | 28 | 1 |
| `cat` | concatenate and print | `view/cat.go` | migrated | 0 | 2 | 1 |
| `code` | Read from stdin (e.g. | `view/code.go` | migrated | 0 | 24 | 1 |
| `cot` | Command-line utility for CotEditor | `view/cot.go` | migrated | 0 | 0 | 1 |
| `du` | estimate file space usage | `view/du.go` | migrated | 0 | 3 | 1 |
| `emacs` | An extensible, customizable, free/libre text editor - and more | `view/emacs.go` | migrated | 0 | 0 | 1 |
| `file` | determine file type | `view/file.go` | migrated | 0 | 0 | 1 |
| `glow` | Render markdown on the CLI, with pizzazz! | `view/glow.go` | migrated | 3 | 9 | 1 |
| `head` | output first lines of file | `view/head.go` | migrated | 0 | 1 | 1 |
| `idea` | IntelliJ IDEA CLI | `view/idea.go` | migrated | 4 | 8 | 1 |
| `less` | view file contents (scrollable) | `view/less.go` | migrated | 0 | 0 | 1 |
| `lvim` | Hyperextensible Vim-based text editor | `view/lvim.go` | migrated | 0 | 29 | 1 |
| `micro` | True/false | `view/micro.go` | migrated | 5 | 40 | 1 |
| `more` | Opposite of less | `view/more.go` | migrated | 0 | 8 | 1 |
| `nano` | Nano | `view/nano.go` | migrated | 0 | 0 | 1 |
| `nvim` | Hyperextensible Vim-based text editor | `view/nvim.go` | migrated | 0 | 29 | 1 |
| `rich` | Defined by terminal, appearance may differ | `view/rich.go` | migrated | 0 | 40 | 1 |
| `stat` | display file status | `view/stat.go` | migrated | 0 | 0 | 1 |
| `subl` | Sublime Text | `view/subl.go` | migrated | 0 | 11 | 1 |
| `tail` | output last lines of file | `view/tail.go` | migrated | 0 | 2 | 1 |
| `vi` | Print help message for vi and exit | `view/vi.go` | migrated | 0 | 1 | 1 |
| `vim` | Vi IMproved, a programmer | `view/vim.go` | migrated | 0 | 38 | 3 |
| `vimr` | VimR - Neovim GUI for macOS in Swift | `view/vimr.go` | migrated | 0 | 7 | 1 |
| `wc` | word, line, character count | `view/wc.go` | migrated | 0 | 3 | 1 |
| `xed` | Xcode text editor invocation tool | `view/xed.go` | migrated | 0 | 5 | 1 |
| `xxd` | Make a hexdump or do the reverse | `view/xxd.go` | migrated | 0 | 16 | 1 |
| `zed` | A lightning-fast, collaborative code editor written in Rust | `view/zed.go` | migrated | 0 | 4 | 1 |

## Text processing, JSON, and stream manipulation

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `awk` | pattern-directed scanning | `text/textproc.go` | migrated | 0 | 6 | 1 |
| `cut` | extract columns from lines | `text/textproc.go` | migrated | 0 | 4 | 1 |
| `diff` | Compare files line by line | `text/diff.go` | migrated | 0 | 40 | 0 |
| `dos2unix` | DOS to Unix file format converter | `text/dos2unix.go` | migrated | 0 | 29 | 0 |
| `egrep` | grep with extended regex | `text/grep.go` | migrated | 0 | 9 | 1 |
| `fd` | fast find alternative | `text/rg.go` | migrated | 0 | 16 | 1 |
| `find` | search for files | `text/find.go` | migrated | 0 | 25 | 1 |
| `gawk` | GNU awk | `text/textproc.go` | migrated | 0 | 6 | 1 |
| `grep` | search text in files | `text/grep.go` | migrated | 0 | 24 | 1 |
| `iconv` | Character set conversion | `text/iconv.go` | migrated | 0 | 9 | 0 |
| `jq` | Output the jq version and exit with zero | `text/jq.go` | migrated | 0 | 15 | 2 |
| `pandoc` | A universal document converter | `text/pandoc.go` | migrated | 0 | 40 | 0 |
| `rg` | ripgrep (fast search) | `text/rg.go` | migrated | 0 | 39 | 1 |
| `sed` | stream editor | `text/textproc.go` | migrated | 0 | 8 | 1 |
| `seq` | Print sequences of numbers. (Defaults to increments of 1) | `text/seq.go` | migrated | 0 | 3 | 0 |
| `sha1sum` | Print or check SHA1 (160-bit) checksums | `text/sha1sum.go` | migrated | 0 | 12 | 0 |
| `shasum` | Print or Check SHA Checksums | `text/shasum.go` | migrated | 0 | 14 | 0 |
| `shred` | Overwrite a file to hide its contents, and optionally delete it | `text/shred.go` | migrated | 0 | 9 | 0 |
| `sort` | sort lines of text | `text/textproc.go` | migrated | 0 | 10 | 1 |
| `split` | Use suffix_length letters to form the suffix of the file name | `text/split.go` | migrated | 0 | 5 | 0 |
| `tee` | read stdin, write to stdout and files | `text/textproc.go` | migrated | 0 | 2 | 1 |
| `tr` | translate or delete characters | `text/textproc.go` | migrated | 0 | 3 | 0 |
| `truncate` | Shrink or extend the size of a file to the specified size | `text/truncate.go` | migrated | 0 | 6 | 0 |
| `typos` | Source code spelling correction | `text/typos.go` | migrated | 0 | 28 | 0 |
| `uniq` | filter adjacent duplicate lines | `text/textproc.go` | migrated | 0 | 6 | 1 |
| `unix2dos` | Unix to DOS text file format convertor | `text/unix2dos.go` | migrated | 0 | 0 | 0 |
| `vale` | A syntax-aware linter for prose built with speed and extensibility in mind | `text/vale.go` | migrated | 3 | 10 | 0 |
| `xargs` | build and run commands from stdin | `text/textproc.go` | migrated | 0 | 9 | 0 |

## Task runners and build automation

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `ant` | Apache Ant - Java library and command-line build tool | `runner/ant.go` | migrated | 0 | 26 | 0 |
| `composer` | Composer Command | `runner/composer.go` | migrated | 0 | 15 | 0 |
| `dbt` | CLI for dbt - Data Build Tool | `runner/dbt.go` | migrated | 28 | 31 | 0 |
| `drush` | Drush is a command line shell and Unix scripting interface for Drupal | `runner/drush.go` | migrated | 0 | 0 | 0 |
| `elixir` | Elixir Language | `runner/elixir.go` | migrated | 0 | 24 | 0 |
| `gem` | Use HTTP proxy for remote operations | `runner/gem.go` | migrated | 50 | 40 | 0 |
| `hexo` | Draft for | `runner/hexo.go` | migrated | 24 | 22 | 0 |
| `just` | command runner | `runner/justfile.go` | migrated | 0 | 14 | 3 |
| `laravel` | The output format (txt, xml, json, or md) | `runner/laravel.go` | migrated | 1 | 16 | 0 |
| `magento` | Open-source E-commerce | `runner/magento.go` | migrated | 0 | 0 | 0 |
| `make` | build automation | `runner/makefile.go` | migrated | 0 | 1 | 1 |
| `mix` | Build tool for Elixir | `runner/mix.go` | migrated | 4 | 19 | 0 |
| `php` | Run the PHP interpreter | `runner/php.go` | migrated | 0 | 0 | 0 |
| `phpunit` | Generate code coverage report in Clover XML format, | `runner/phpunit.go` | migrated | 0 | 38 | 0 |
| `phpunit-watcher` | Automatically rerun PHPUnit tests when source code changes | `runner/phpunit_watcher.go` | migrated | 0 | 1 | 0 |
| `rails` | Create a new rails application | `runner/rails.go` | migrated | 0 | 40 | 0 |
| `rake` | A ruby build program with capabilities similar to make | `runner/rake.go` | migrated | 0 | 12 | 0 |
| `rubocop` | Run only lint cops | `runner/rubocop.go` | migrated | 0 | 38 | 0 |
| `ruby` | Interpreted object-oriented scripting language | `runner/ruby.go` | migrated | 0 | 13 | 0 |
| `rvm` | Show version of rvm | `runner/rvm.go` | migrated | 13 | 8 | 0 |
| `sidekiq` | Background job framework for Ruby | `runner/sidekiq.go` | migrated | 0 | 10 | 0 |
| `symfony` | Symfony Binary | `runner/symfony.go` | migrated | 50 | 6 | 0 |
| `valet` | Do not output any message | `runner/valet.go` | migrated | 36 | 30 | 0 |
| `vapor` | Vapor Toolbox (Server-side Swift web framework) | `runner/vapor.go` | migrated | 11 | 8 | 0 |

## System administration, network, and process management

| Command | Description | IRIS source | Status | Subcommands | Options | Generators |
| --- | --- | --- | --- | ---: | ---: | ---: |
| `adb` | Forward-lock the app | `sys/adb.go` | migrated | 50 | 18 | 0 |
| `ag` | Recursively search for PATTERN in PATH. Like grep or ack, but faster | `sys/ag.go` | migrated | 0 | 40 | 0 |
| `airflow` | Subcommand | `sys/airflow.go` | migrated | 49 | 40 | 0 |
| `aliases` | Prints help information | `sys/aliases.go` | migrated | 20 | 7 | 0 |
| `asciinema` | Terminal session recorder | `sys/asciinema.go` | migrated | 7 | 15 | 0 |
| `asr` | Can be a disk image, /dev entry, or volume mountpoint | `sys/asr.go` | migrated | 7 | 20 | 0 |
| `atuin` | Magical shell history | `sys/atuin.go` | migrated | 18 | 30 | 0 |
| `basename` | Return filename portion of pathname | `sys/basename.go` | migrated | 0 | 2 | 0 |
| `bc` | An arbitrary precision calculator language | `sys/bc.go` | migrated | 0 | 7 | 0 |
| `btop` | Beautifuler htop (interactive process viewer) | `sys/btop.go` | migrated | 0 | 7 | 0 |
| `bundle` | Gem | `sys/bundle.go` | migrated | 16 | 40 | 0 |
| `cal` | Displays a calendar and the date of Easter | `sys/cal.go` | migrated | 0 | 3 | 0 |
| `cci` | CumulusCI command line interface | `sys/cci.go` | migrated | 39 | 36 | 0 |
| `cdk8s` | CDK for K8s | `sys/cdk8s.go` | migrated | 5 | 13 | 0 |
| `chezmoi` | Attribute modifier | `sys/chezmoi.go` | migrated | 43 | 40 | 0 |
| `chsh` | Change your login shell | `sys/chsh.go` | migrated | 0 | 4 | 0 |
| `codesign` | Create and manipulate code signatures | `sys/codesign.go` | migrated | 0 | 6 | 0 |
| `croc` | Send file(s), or folder | `sys/croc.go` | migrated | 3 | 26 | 0 |
| `crontab` | Maintain crontab file for individual users | `sys/crontab.go` | migrated | 0 | 4 | 0 |
| `curl` | transfer data via URL | `sys/network.go` | migrated | 0 | 22 | 1 |
| `date` | Display or set date and time | `sys/date.go` | migrated | 0 | 9 | 0 |
| `dateseq` | Print help and exit | `sys/dateseq.go` | migrated | 0 | 4 | 0 |
| `dcli` | Display help for command | `sys/dcli.go` | migrated | 26 | 15 | 0 |
| `dd` | The same as | `sys/dd.go` | migrated | 0 | 0 | 0 |
| `ddev` | DDEV-Local local development environment | `sys/ddev.go` | migrated | 44 | 40 | 0 |
| `defaults` | Global domain | `sys/defaults.go` | migrated | 9 | 3 | 0 |
| `degit` | Straightforward project scaffolding | `sys/degit.go` | migrated | 0 | 5 | 0 |
| `deta` | Runtime | `sys/deta.go` | migrated | 31 | 13 | 0 |
| `dig` | DNS lookup | `sys/network.go` | migrated | 4 | 1 | 0 |
| `dirname` | Return directory portion of pathname | `sys/dirname.go` | migrated | 0 | 0 | 0 |
| `do-release-upgrade` | Upgrade Ubuntu to latest release | `sys/do_release_upgrade.go` | migrated | 0 | 7 | 0 |
| `dog` | Human-readable host names, nameservers, types, or classes | `sys/dog.go` | migrated | 7 | 16 | 0 |
| `dotnet` | The dotnet cli | `sys/dotnet.go` | migrated | 0 | 5 | 0 |
| `dscacheutil` | Utility for managing the Directory Service cache | `sys/dscacheutil.go` | migrated | 1 | 8 | 0 |
| `dscl` | Prompt for password | `sys/dscl.go` | migrated | 13 | 8 | 0 |
| `dtm` | Plugin | `sys/dtm.go` | migrated | 20 | 16 | 0 |
| `echo` | Environment Variable | `sys/echo.go` | migrated | 0 | 3 | 0 |
| `eleventy` | Eleventy is a simpler static site generator | `sys/eleventy.go` | migrated | 0 | 0 | 0 |
| `env` | print environment | `sys/env.go` | migrated | 0 | 3 | 1 |
| `exec` | Replace the current shell with a program | `sys/exec.go` | migrated | 0 | 0 | 0 |
| `export` | set environment variable | `sys/env.go` | migrated | 0 | 0 | 1 |
| `fastlane` | Helps you with your initial fastlane setup | `sys/fastlane.go` | migrated | 24 | 15 | 0 |
| `fdisk` | Manipulate disk partition table | `sys/fdisk.go` | migrated | 0 | 15 | 0 |
| `ffmpeg` | Play, record, convert, and stream audio and video | `sys/ffmpeg.go` | migrated | 0 | 40 | 0 |
| `firefox` | Free open-source web browser developer by Mozilla | `sys/firefox.go` | migrated | 0 | 35 | 0 |
| `fisher` | [Prompt] - 🌊 The ultimate Fish prompt | `sys/fisher.go` | migrated | 5 | 2 | 0 |
| `fmt` | Simple text formatter | `sys/fmt.go` | migrated | 0 | 1 | 0 |
| `forc` | Fuel Orchestrator | `sys/forc.go` | migrated | 30 | 21 | 0 |
| `forge` | A command line interface for managing Atlassian-hosted apps | `sys/forge.go` | migrated | 21 | 16 | 0 |
| `fzf` | A general-purpose command-line fuzzy finder | `sys/fzf.go` | migrated | 0 | 40 | 0 |
| `fzf-tmux` | Opens a fuzzy finder in a tmux pane | `sys/fzf_tmux.go` | migrated | 0 | 40 | 0 |
| `gltfjsx` | GLTF to JSX converter | `sys/gltfjsx.go` | migrated | 0 | 9 | 0 |
| `goto` | Goto | `sys/goto.go` | migrated | 0 | 9 | 0 |
| `gum` | Background Color | `sys/gum.go` | migrated | 17 | 40 | 0 |
| `herd` | Display this application version | `sys/herd.go` | migrated | 6 | 19 | 0 |
| `hop` | Interact with Hop in your terminal | `sys/hop.go` | migrated | 14 | 10 | 0 |
| `hostname` | Set or print name of current host system | `sys/hostname.go` | migrated | 0 | 3 | 0 |
| `htop` | Improved top (interactive process viewer) | `sys/htop.go` | migrated | 0 | 12 | 0 |
| `http` | HTTPie: command-line HTTP client for the API era | `sys/http.go` | migrated | 0 | 22 | 0 |
| `hyper` | Hyper is an Electron-based terminal | `sys/hyper.go` | migrated | 9 | 0 | 0 |
| `hyperfine` | A command-line benchmarking tool | `sys/hyperfine.go` | migrated | 0 | 18 | 0 |
| `ibus` | Set or get engine | `sys/ibus.go` | migrated | 12 | 0 | 0 |
| `id` | Display the full name of the user | `sys/id.go` | migrated | 0 | 6 | 0 |
| `ifconfig` | configure network interface | `sys/network.go` | migrated | 0 | 1 | 0 |
| `ignite-cli` | Output usage information | `sys/ignite_cli.go` | migrated | 4 | 7 | 0 |
| `install` | Use suffix as the backup suffix if -b is given | `sys/install.go` | migrated | 0 | 6 | 0 |
| `ip` | show/manage network | `sys/network.go` | migrated | 5 | 4 | 0 |
| `join` | The join utility performs an | `sys/join.go` | migrated | 0 | 4 | 0 |
| `julia` | The Julia Programming Language | `sys/julia.go` | migrated | 0 | 37 | 0 |
| `kafkactl` | Command-line interface for Apache Kafka | `sys/kafkactl.go` | migrated | 27 | 40 | 0 |
| `kamal` | Skip image build and push | `sys/kamal.go` | migrated | 11 | 21 | 0 |
| `kill` | send signal to process | `sys/ps.go` | migrated | 0 | 6 | 1 |
| `killall` | kill by process name | `sys/ps.go` | migrated | 0 | 4 | 1 |
| `kitty` | A cat like utility to display images in the terminal | `sys/kitty.go` | migrated | 11 | 30 | 0 |
| `klist` | Credential cache to list | `sys/klist.go` | migrated | 0 | 8 | 0 |
| `kool` | Script | `sys/kool.go` | migrated | 20 | 16 | 0 |
| `launchctl` | Service or domain target | `sys/launchctl.go` | migrated | 49 | 23 | 0 |
| `leaf` | Create and interact with your leaf projects | `sys/leaf.go` | migrated | 13 | 8 | 0 |
| `lima` | Lima is an alias for | `sys/lima.go` | migrated | 0 | 1 | 0 |
| `login` | Begin session on the system | `sys/login.go` | migrated | 0 | 4 | 0 |
| `lsblk` | List block devices | `sys/lsblk.go` | migrated | 0 | 25 | 0 |
| `lsof` | List open files | `sys/lsof.go` | migrated | 0 | 31 | 0 |
| `man` | Format and display manual pages | `sys/man.go` | migrated | 0 | 20 | 0 |
| `meroxa` | The Meroxa CLI | `sys/meroxa.go` | migrated | 32 | 37 | 0 |
| `mkdocs` | Project documentation with Markdown | `sys/mkdocs.go` | migrated | 4 | 19 | 0 |
| `mkfifo` | Make FIFOs (first-in, first-out) | `sys/mkfifo.go` | migrated | 0 | 1 | 0 |
| `mkinitcpio` | Create an initial ramdisk environment | `sys/mkinitcpio.go` | migrated | 0 | 26 | 0 |
| `mknod` | Create device special file | `sys/mknod.go` | migrated | 2 | 1 | 0 |
| `mosh` | Address of remote machine to log into | `sys/mosh.go` | migrated | 0 | 11 | 0 |
| `mount` | Mount disks and manage subtrees | `sys/mount.go` | migrated | 0 | 34 | 0 |
| `nc` | netcat - TCP/UDP tool | `sys/network.go` | migrated | 0 | 7 | 0 |
| `ncal` | Displays a calendar and the date of Easter | `sys/ncal.go` | migrated | 0 | 13 | 0 |
| `neofetch` | The most complete system information CLI tool | `sys/neofetch.go` | migrated | 0 | 40 | 0 |
| `netstat` | network statistics | `sys/network.go` | migrated | 0 | 7 | 0 |
| `networkQuality` | Measure the different aspects of network quality | `sys/networkquality.go` | migrated | 0 | 6 | 0 |
| `networksetup` | Configuration tool for network settings in macOS | `sys/networksetup.go` | migrated | 39 | 40 | 0 |
| `nextflow` | Session ID | `sys/nextflow.go` | migrated | 17 | 40 | 0 |
| `nhost` | Nhost | `sys/nhost.go` | migrated | 10 | 1 | 0 |
| `nmap` | Network exploration tool and security / port scanner | `sys/nmap.go` | migrated | 0 | 21 | 0 |
| `nrm` | Use the right package manage - remove | `sys/nrm.go` | migrated | 0 | 5 | 0 |
| `ns` | Forces rebuilding the native application | `sys/ns.go` | migrated | 1 | 35 | 0 |
| `nslookup` | query DNS | `sys/network.go` | migrated | 0 | 0 | 0 |
| `nylas` | A command line interface for Nylas | `sys/nylas.go` | migrated | 50 | 40 | 0 |
| `oh-my-posh` | The config file to use | `sys/oh_my_posh.go` | migrated | 13 | 14 | 0 |
| `okta` | The Okta CLI is the easiest way to get started with Okta! | `sys/okta.go` | migrated | 11 | 14 | 0 |
| `ollama` | A command-line tool for managing and deploying machine learning models | `sys/ollama.go` | migrated | 12 | 4 | 0 |
| `omz` | Oh My Zsh | `sys/omz.go` | migrated | 17 | 0 | 0 |
| `pac` | 7 | `sys/pac.go` | migrated | 50 | 40 | 0 |
| `passwd` | Modify a user | `sys/passwd.go` | migrated | 0 | 3 | 0 |
| `pathchk` | Check pathnames for POSIX portability | `sys/pathchk.go` | migrated | 0 | 1 | 0 |
| `pdfunite` | Combine multiple pdfs | `sys/pdfunite.go` | migrated | 0 | 2 | 0 |
| `pgrep` | find process by pattern | `sys/ps.go` | migrated | 0 | 4 | 0 |
| `ping` | test network connectivity | `sys/network.go` | migrated | 0 | 5 | 0 |
| `pkg-config` | Return metainformation about installed libraries | `sys/pkg_config.go` | migrated | 0 | 20 | 0 |
| `pkill` | kill by pattern | `sys/ps.go` | migrated | 0 | 3 | 0 |
| `pmset` | Display sleep timer (value in minutes, or 0 to disable) | `sys/pmset.go` | migrated | 21 | 5 | 0 |
| `pocketbase` | PocketBase CLI | `sys/pocketbase.go` | migrated | 11 | 8 | 0 |
| `printenv` | print environment variables | `sys/env.go` | migrated | 0 | 1 | 1 |
| `prisma` | Display this help message | `sys/prisma.go` | migrated | 9 | 36 | 0 |
| `pro` | Manage Ubuntu Pro services from Canonical | `sys/pro.go` | migrated | 8 | 5 | 0 |
| `pry` | Interactive Ruby | `sys/pry.go` | migrated | 0 | 16 | 0 |
| `ps` | report processes | `sys/ps.go` | migrated | 1 | 6 | 1 |
| `publish` | Set up a new website in the current folder | `sys/publish.go` | migrated | 3 | 2 | 0 |
| `pwd` | Return working directory name | `sys/pwd.go` | migrated | 0 | 2 | 0 |
| `rancher` | Output format: | `sys/rancher.go` | migrated | 46 | 16 | 0 |
| `repeat` | Interpret the result as a number and repeat the commands this many times | `sys/repeat.go` | migrated | 0 | 0 | 0 |
| `rscript` | Scripting Front-End for R | `sys/rscript.go` | migrated | 0 | 13 | 0 |
| `sam` | Host of locally emulated Lambda container | `sys/sam.go` | migrated | 17 | 40 | 0 |
| `sanity` | Displays help information about Sanity | `sys/sanity.go` | migrated | 38 | 40 | 0 |
| `screen` | Screen manager with VT100/ANSI terminal emulation | `sys/screen.go` | migrated | 1 | 29 | 0 |
| `shell-config` | Display help for command | `sys/shell_config.go` | migrated | 10 | 4 | 0 |
| `shortcuts` | Run a shortcut | `sys/shortcuts.go` | migrated | 1 | 8 | 0 |
| `simctl` | Add photos, live photos, videos, or contacts to the library of a device | `sys/simctl.go` | migrated | 48 | 29 | 0 |
| `source` | Source files in shell | `sys/source.go` | migrated | 0 | 0 | 0 |
| `speedtest-cli` | Command line interface for testing internet bandwidth using speedtest.net | `sys/speedtest_cli.go` | migrated | 0 | 14 | 0 |
| `spotify` | CLI to use Spotify from the terminal | `sys/spotify.go` | migrated | 23 | 1 | 0 |
| `ss` | socket statistics | `sys/network.go` | migrated | 0 | 8 | 0 |
| `st2` | Show this help and exit | `sys/st2.go` | migrated | 8 | 40 | 0 |
| `stack` | The Haskell Tool Stack | `sys/stack.go` | migrated | 15 | 35 | 0 |
| `starkli` | Starkli, a ⚡ blazing ⚡ fast ⚡ CLI tool for Starknet powered by 🦀 starknet-rs 🦀 | `sys/starkli.go` | migrated | 49 | 27 | 0 |
| `su` | (no letter) The same as -l | `sys/su.go` | migrated | 0 | 1 | 0 |
| `sudo` | Execute a command as the superuser or another user | `sys/sudo.go` | migrated | 0 | 3 | 0 |
| `sysctl` | Variable name | `sys/sysctl.go` | migrated | 0 | 11 | 0 |
| `systemctl` | Control the systemd system and service manager | `sys/systemctl.go` | migrated | 51 | 46 | 14 |
| `tac` | Concatenate and print files in reverse | `sys/tac.go` | migrated | 0 | 5 | 0 |
| `tailcall` | TailCall CLI for managing and optimizing GraphQL configurations | `sys/tailcall.go` | migrated | 4 | 3 | 0 |
| `tailwindcss` | Display usage information | `sys/tailwindcss.go` | migrated | 2 | 11 | 0 |
| `time` | Time how long a command takes! | `sys/time.go` | migrated | 0 | 0 | 0 |
| `tldr` | Tldr page | `sys/tldr.go` | migrated | 0 | 9 | 0 |
| `tmux` | Format output | `sys/tmux.go` | migrated | 50 | 40 | 0 |
| `tmuxinator` | Project | `sys/tmuxinator.go` | migrated | 23 | 4 | 0 |
| `top` | Display Linux tasks | `sys/top.go` | migrated | 0 | 5 | 0 |
| `traceroute` | Print the route packets take to network host | `sys/traceroute.go` | migrated | 0 | 11 | 0 |
| `trap` | Prints all defined signal handlers | `sys/trap.go` | migrated | 0 | 2 | 0 |
| `trex` | trex script | `sys/trex.go` | migrated | 9 | 11 | 0 |
| `tsh` | Remote host login | `sys/tsh.go` | migrated | 26 | 15 | 0 |
| `tuist` | Build the project in the current directory | `sys/tuist.go` | migrated | 5 | 19 | 0 |
| `twilio` | Level of logging messages | `sys/twilio.go` | migrated | 2 | 39 | 0 |
| `uname` | Print operating system name | `sys/uname.go` | migrated | 0 | 7 | 0 |
| `unset` | unset variable | `sys/env.go` | migrated | 0 | 0 | 1 |
| `visudo` | Checking existing sudoers file for syntax errors | `sys/visudo.go` | migrated | 0 | 9 | 0 |
| `vultr-cli` | Bare Metal ID | `sys/vultr_cli.go` | migrated | 50 | 4 | 0 |
| `wezterm` | Wez | `sys/wezterm.go` | migrated | 17 | 27 | 0 |
| `wget` | non-interactive downloader | `sys/network.go` | migrated | 0 | 13 | 1 |
| `where` | For each name, indicate how it should be interpreted | `sys/where.go` | migrated | 0 | 6 | 0 |
| `whereis` | Locate the binary, source, and manual page files for a command | `sys/whereis.go` | migrated | 0 | 8 | 0 |
| `which` | Executable file | `sys/which.go` | migrated | 0 | 2 | 0 |
| `who` | Display who is logged in | `sys/who.go` | migrated | 1 | 12 | 0 |
| `wing` | Runs a Wing executable in the Wing Console | `sys/wing.go` | migrated | 6 | 3 | 0 |
| `wp` | Path to the WordPress files | `sys/wp.go` | migrated | 50 | 40 | 0 |
| `wrk` | Wrk - a HTTP benchmarking tool | `sys/wrk.go` | migrated | 0 | 9 | 0 |
| `wscat` | Communicate over websocket | `sys/wscat.go` | migrated | 0 | 21 | 0 |
| `yank` | Yank terminal output to clipboard | `sys/yank.go` | migrated | 0 | 6 | 0 |
| `ykman` | Configure your YubiKey via the command line | `sys/ykman.go` | migrated | 50 | 39 | 0 |
| `zapier` | Change the way structured data is presented. If | `sys/zapier.go` | migrated | 40 | 14 | 0 |

## Supplemental runtime specifications

These live IRIS registry roots were absent from its generated inventory, or are Argmax-specific local integrations.

| Command | Description | Subcommands | Options | Generators |
| --- | --- | ---: | ---: | ---: |
| `pip` | python packages | 18 | 13 | 4 |
| `pip3` | python packages | 12 | 9 | 2 |
| `py` | python interpreter | 0 | 8 | 1 |
| `python` | python interpreter | 0 | 8 | 1 |
| `python3` | python interpreter | 0 | 8 | 1 |
| `ssh-add` | load keys into an SSH agent | 0 | 0 | 0 |
| `ssh-agent` | hold private keys for SSH clients | 0 | 0 | 0 |
| `ssh-keyscan` | collect public SSH host keys | 0 | 0 | 0 |
| `sshd` | OpenSSH server daemon | 0 | 0 | 0 |
| `zoxide` | query the zoxide directory database | 3 | 0 | 1 |
