{
  description = "tf2demos — organize Team Fortress 2 demos (ds_* recordings)";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # winit/glutin dlopen these at run time (they are not DT_NEEDED), so both the devShell
      # and the installed binary need them on LD_LIBRARY_PATH.
      runtimeLibs = pkgs: with pkgs; [
        wayland
        libxkbcommon
        libGL
        vulkan-loader
        fontconfig
      ];
    in
    {
      packages = forAllSystems (pkgs: rec {
        tf2demos = pkgs.rustPlatform.buildRustPackage {
          pname = "tf2demos";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./src
              ./tests
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.makeWrapper
          ];
          buildInputs = runtimeLibs pkgs;
          postFixup = ''
            wrapProgram $out/bin/tf2demos \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)}
          '';
          # `cargo test` runs in checkPhase (doCheck defaults to true).
          meta = {
            description = "Organize Team Fortress 2 demos recorded by the built-in ds_* support";
            mainProgram = "tf2demos";
            license = pkgs.lib.licenses.mit;
          };
        };
        default = tf2demos;
      });

      checks = forAllSystems (pkgs: {
        # Unit tests run inside the package build.
        tests = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        clippy = self.packages.${pkgs.stdenv.hostPlatform.system}.default.overrideAttrs (old: {
          pname = "tf2demos-clippy";
          nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
          buildPhase = ''
            runHook preBuild
            cargo clippy --all-targets --offline -- -D warnings
            runHook postBuild
          '';
          doCheck = false;
          installPhase = "touch $out";
          # Nothing to wrap: the package's wrapProgram would fail on the empty output.
          postFixup = "";
        });
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages =
            (with pkgs; [
              cargo
              rustc
              rust-analyzer
              clippy
              rustfmt
              pkg-config
            ])
            ++ runtimeLibs pkgs;
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });

      homeManagerModules.default = import ./nix/hm-module.nix { inherit self; };
    };
}
