{
  description = "Simple clips viewer for the people";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    systems.url = "github:nix-systems/default";
    flake-utils.url = "github:numtide/flake-utils";

    treefmt-nix.url = "github:numtide/treefmt-nix";

    crane.url = "github:ipetkov/crane";
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      systems,
      flake-utils,
      treefmt-nix,
      crane,
      advisory-db,
      rust-overlay,
    }:
    flake-utils.lib.eachSystem (import systems) (
      system:
      let
        pkgs = (import nixpkgs) {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        inherit (pkgs) lib;

        cargoToml = fromTOML (builtins.readFile ./Cargo.toml);
        pname = cargoToml.package.name;
        craneLib = (crane.mkLib pkgs).overrideToolchain (
          p: p.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
        );

        buildInputs = with pkgs; [
          vulkan-loader
          fontconfig
          libxkbcommon
          wayland
          file
        ];
        nativeBuildInputs = with pkgs; [
          pkg-config
          makeWrapper
        ];

        src = craneLib.cleanCargoSource ./.;
        commonArgs = {
          inherit src;
          strictDeps = true;

          inherit buildInputs nativeBuildInputs;
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        package = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            meta.description = cargoToml.package.description;
            postFixup = ''
              wrapProgram $out/bin/${pname} \
              --suffix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath buildInputs}
            '';
          }
        );

        treefmtEval = treefmt-nix.lib.evalModule pkgs ./treefmt.nix;
      in
      {
        formatter = treefmtEval.config.build.wrapper;

        checks = {
          ${pname} = package;
          formatting = treefmtEval.config.build.check self;
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
          audit = craneLib.cargoAudit { inherit src advisory-db; };
          nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
              cargoNextestPartitionsExtraArgs = "--no-tests=pass";
            }
          );
        };

        packages = {
          ${pname} = package;
          default = package;
        };

        devShells.default = craneLib.devShell {
          packages = buildInputs ++ nativeBuildInputs ++ (with pkgs; [ rust-analyzer ]);
          LD_LIBRARY_PATH = lib.makeLibraryPath buildInputs;
          RUST_BACKTRACE = 1;
        };
      }
    );
}
