# Development tasks for argmax.

scratch := `mktemp -d 2>/dev/null || echo /tmp`

# Build the binary.
build:
    go build -o argmax ./cmd/argmax

# Run formatters, linters, and the full test suite for both Go modules.
check: fmt lint test figexport-check

fmt:
    goimports -w .

lint:
    go vet ./...
    golangci-lint run ./...

test:
    go test -race ./...

figexport-check:
    cd tools/figexport && go vet ./...
    cd tools/figexport && go test -race ./...

bench:
    go test -run NONE -bench . -benchtime 10x ./...

# Regenerate the catalog data bundle and documentation from a Fig corpus
# dump directory (see tools/figexport).
generate figjson:
    go run ./tools/cataloggen -prd argmax-prd.md -fig {{figjson}} -out internal/catalog/data
    go run ./tools/docgen

# Regenerate the generated docs only.
docs:
    go run ./tools/docgen

# Build and run a wrapped session with the local binary.
run: build
    ./argmax
