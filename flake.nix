# Release template only: this intentionally fails to build until a release
# maintainer pins flake.lock and replaces vendorHash with the real dependency
# hash for the tagged source. See packaging/README.md.
{
  description = "argmax — terminal-resident command completion and prediction";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        packages.default = pkgs.buildGoModule {
          pname = "argmax";
          version = "0.1.0";
          src = ./.;
          subPackages = [ "cmd/argmax" ];
          # TEMPLATE PLACEHOLDER: replace with the hash reported by the
          # first build before advertising this as an installation method.
          vendorHash = pkgs.lib.fakeHash;
          env.CGO_ENABLED = 0;
          ldflags = [ "-s" "-w" "-X main.version=0.1.0" ];
          meta = with pkgs.lib; {
            description = "Terminal-resident command completion and prediction";
            homepage = "https://github.com/rselbach/argmax";
            license = licenses.mit;
            mainProgram = "argmax";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ go golangci-lint goreleaser just ];
        };
      });
}
