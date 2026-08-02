# Development tasks for argmax.

# Build the binary.
build:
    go build -o argmax ./cmd/argmax

# Run formatters, linters, and the full test suite.
check: fmt lint test

fmt:
    gofmt -w .

lint:
    go vet ./...
    golangci-lint run ./...

test:
    go test -race ./...

bench:
    go test -run NONE -bench . -benchtime 10x ./...

# Regenerate the catalog documentation (docs/commands.md).
docs:
    go run ./tools/docgen

# Verify the catalog documentation is current (CI drift gate).
docs-check:
    go run ./tools/docgen -check

# Build and run a wrapped session with the local binary.
run: build
    ./argmax
