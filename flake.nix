{
  description = "isflac — fake FLAC detector";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
            gcc
            pkg-config
          ];

          buildInputs = [ ];

          shellHook = ''
            export PATH="${pkgs.cargo}/bin:${pkgs.rustc}/bin:$PATH"
            echo "rust dev shell: $(rustc --version)"
          '';
        };
      });
}
