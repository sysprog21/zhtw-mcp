{
  description = "MCP server for Traditional Chinese (zh-TW) text linting and normalization";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.1"; # unstable Nixpkgs
    fenix = {
      url = "https://flakehub.com/f/nix-community/fenix/0.1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, ... }@inputs:

    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forEachSupportedSystem =
        f:
        inputs.nixpkgs.lib.genAttrs supportedSystems (
          system:
          f {
            inherit system;
            pkgs = import inputs.nixpkgs {
              inherit system;
              overlays = [
                self.overlays.default
              ];
            };
          }
        );
    in
    {
      overlays.default = final: prev: {
        rustToolchain =
          with inputs.fenix.packages.${final.stdenv.hostPlatform.system};
          combine (
            with stable;
            [
              clippy
              rustc
              cargo
              rustfmt
              rust-src
            ]
          );

        zhtw-mcp =
          let
            openccRev = "0ad13e022313ab62daf9b7ef79047b2d084a8868";
            openccDictUrl =
              file: "https://raw.githubusercontent.com/BYVoid/OpenCC/${openccRev}/data/dictionary/${file}";
            openccDicts = final.linkFarm "opencc-dictionaries" [
              {
                name = "STPhrases.txt";
                path = final.fetchurl {
                  url = openccDictUrl "STPhrases.txt";
                  hash = "sha256-nC1CoWSP9+x+d003fHpz9yECr6TmGMLZ3dlfkvq2lgA=";
                };
              }
              {
                name = "STCharacters.txt";
                path = final.fetchurl {
                  url = openccDictUrl "STCharacters.txt";
                  hash = "sha256-nO37i/E6IgCHED2altn1YFDDQcJKgJy85chckEVFZVc=";
                };
              }
              {
                name = "TWVariants.txt";
                path = final.fetchurl {
                  url = openccDictUrl "TWVariants.txt";
                  hash = "sha256-iUc+luP2HpvT8uMDudiKycqmHv+x+q3O+U/15luO1Us=";
                };
              }
            ];
            rustPlatform = final.makeRustPlatform {
              cargo = final.rustToolchain;
              rustc = final.rustToolchain;
            };
          in
          rustPlatform.buildRustPackage (finalAttrs: {
            pname = "zhtw-mcp";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

            src = final.lib.cleanSource ./.;

            cargoHash = "sha256-vwguHA1z9ne6P8Q+JBMafAIkhihMgBOdxbHF0HrWLes=";

            nativeBuildInputs = [
              final.python3
              final.rustToolchain
            ];

            preBuild = ''
              mkdir -p data/opencc
              cp ${openccDicts}/STPhrases.txt data/opencc/STPhrases.txt
              cp ${openccDicts}/STCharacters.txt data/opencc/STCharacters.txt
              cp ${openccDicts}/TWVariants.txt data/opencc/TWVariants.txt
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
              platforms = platforms.unix;
            };
          });
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
              cargo-deny
              cargo-edit
              cargo-watch
              rust-analyzer
              self.formatter.${system}
            ];

            env = {
              # Required by rust-analyzer
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
            };
          };
        }
      );

      formatter = forEachSupportedSystem ({ pkgs, ... }: pkgs.nixfmt);
    };
}
