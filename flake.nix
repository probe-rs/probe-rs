{
  description = "probe-rs - A collection of on chip debugging tools";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      flake-utils,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
    in
    {
      # Overlay for easy integration into other flakes
      overlays.default = final: prev: {
        probe-rs = self.packages.${final.system}.probe-rs;
      };
    }
    // flake-utils.lib.eachSystem supportedSystems (
      system:
      let
        pkgs = import nixpkgs { inherit system; };

        # Rust toolchain with all components needed for development
        rustToolchain = fenix.packages.${system}.stable.withComponents [
          "cargo"
          "clippy"
          "rust-src"
          "rustc"
          "rustfmt"
        ];

        # Native build inputs (build-time dependencies)
        nativeBuildInputs = with pkgs; [
          pkg-config
          rustToolchain
        ];

        # Build inputs (runtime library dependencies)
        buildInputs =
          with pkgs;
          [
            libusb1
          ]
          ++ lib.optionals stdenv.isLinux [
            hidapi
            udev
          ]
          ++ lib.optionals stdenv.isDarwin [
            apple-sdk_15
          ];

      in
      {
        packages = {
          probe-rs = pkgs.rustPlatform.buildRustPackage {
            pname = "probe-rs-tools";
            version = "0.30.0";

            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = nativeBuildInputs;
            buildInputs = buildInputs;

            # Only build the probe-rs-tools package
            cargoBuildFlags = [
              "--package"
              "probe-rs-tools"
            ];

            # Skip tests during package build
            doCheck = false;
          };

          default = self.packages.${system}.probe-rs;
        };

        # Development shell
        devShells.default = pkgs.mkShell {
          nativeBuildInputs =
            nativeBuildInputs
            ++ (with pkgs; [
              rust-analyzer
              cargo-zigbuild
            ]);

          buildInputs = buildInputs;

          # Help rust-analyzer find the rust source
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}
