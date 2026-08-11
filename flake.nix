{
  description = "Nix package for zhtw-mcp";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1";
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Must match OPENCC_COMMIT in scripts/gen-s2t-tables.py; pinned in the URL
    # so `nix flake update` cannot move the conversion tables.
    opencc-src = {
      url = "github:BYVoid/OpenCC/5249273a3e5606852f088c9a8b23522145d94f78";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      opencc-src,
    }:

    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forEachSupportedSystem =
        f:
        nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            inherit system;
            pkgs = import nixpkgs {
              inherit system;
              overlays = [ self.overlays.default ];
            };
          }
        );
    in
    {
      overlays.default = final: prev: {
        rustToolchain =
          with fenix.packages.${final.stdenv.hostPlatform.system};
          combine (
            with stable;
            [
              cargo
              clippy
              rust-src
              rustc
              rustfmt
            ]
          );

        zhtw-mcp =
          let
            cargoToml = fromTOML (builtins.readFile ./Cargo.toml);
            rustPlatform = final.makeRustPlatform {
              cargo = final.rustToolchain;
              rustc = final.rustToolchain;
            };
          in
          rustPlatform.buildRustPackage {
            pname = "zhtw-mcp";
            inherit (cargoToml.package) version;

            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = [
              final.python3
              final.rustToolchain
            ];

            # The sandbox has no network, so seed the generator's cache from
            # the pinned input.  Its path is keyed by OPENCC_COMMIT, read from
            # the script rather than hardcoded: a copy landing anywhere else is
            # ignored and the build falls through to a download that must fail.
            preBuild = ''
              pinned=$(sed -n 's/^OPENCC_COMMIT = "\(.*\)"$/\1/p' scripts/gen-s2t-tables.py)
              if [ "$pinned" != "${opencc-src.rev}" ]; then
                echo "error: flake.lock pins OpenCC ${opencc-src.rev}, but" >&2
                echo "  scripts/gen-s2t-tables.py pins $pinned." >&2
                echo "  Re-pin inputs.opencc-src.url in flake.nix, then run:" >&2
                echo "    nix flake lock --override-input opencc-src github:BYVoid/OpenCC/$pinned" >&2
                exit 1
              fi

              cache=data/opencc/''${pinned:0:12}
              mkdir -p "$cache"
              for dict in STPhrases STCharacters TWVariants; do
                cp ${opencc-src}/data/dictionary/$dict.txt "$cache/$dict.txt"
              done

              python3 scripts/gen-s2t-tables.py
              rustfmt src/engine/s2t_data.rs
            '';

            cargoTestFlags = [
              "--lib"
              "--bins"
            ];

            meta = with final.lib; {
              description = "MCP server for Traditional Chinese (zh-TW) text linting and normalization";
              homepage = "https://github.com/sysprog21/zhtw-mcp";
              license = licenses.mit;
              mainProgram = "zhtw-mcp";
            };
          };
      };

      packages = forEachSupportedSystem (
        { pkgs, ... }:
        {
          inherit (pkgs) zhtw-mcp;
          default = pkgs.zhtw-mcp;
        }
      );

      devShells = forEachSupportedSystem (
        { pkgs, system }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain
              openssl
              pkg-config
              python3
              self.formatter.${system}
            ];
          };
        }
      );

      formatter = forEachSupportedSystem ({ pkgs, ... }: pkgs.nixfmt);
    };
}
