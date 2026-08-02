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
          # Replace with the real hash reported by the first build.
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
