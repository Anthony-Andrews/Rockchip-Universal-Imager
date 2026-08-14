{
  # Nix package for NixOS/Linux users:
  #   nix profile install github:Anthony-Andrews/Rockchip-Universal-Imager
  # or, preferred on NixOS (installs the app AND the udev rule for USB access):
  #   inputs.rockchip-universal-imager.url = "github:Anthony-Andrews/Rockchip-Universal-Imager";
  #   # in your system config:
  #   imports = [ inputs.rockchip-universal-imager.nixosModules.default ];
  #   programs.rockchip-universal-imager.enable = true;
  #
  # This is an independent from-source build path — it does not consume the
  # CI-built artifacts. Layout mirrors the portable folder: the app,
  # rkdeveloptool, and loader_binaries/ all land in $out/bin, which satisfies
  # the sibling lookup in src-tauri/src/paths.rs unchanged.
  description = "Rockchip Universal Imager — Rockchip flashing and eMMC helper (Tauri GUI + rkdeveloptool)";

  # Binary cache (pushed by .github/workflows/nix.yaml from the self-hosted
  # runners). Nix offers this to users on first build; accepting it means
  # downloading prebuilt paths instead of compiling. NixOS users can instead
  # set nix.settings.{substituters,trusted-public-keys} declaratively.
  nixConfig = {
    extra-substituters = [ "https://antho.cachix.org" ];
    extra-trusted-public-keys = [
      "antho.cachix.org-1:NU2YLmMqJD1O221PjtpZU0rBpzaoNesspvyQeBra0yc="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Pinned to the SAME commit as the dependencies/rkdeveloptool submodule
    # gitlink. When you move the submodule pin, update this rev to match.
    rkdeveloptool-src = {
      url = "github:Anthony-Andrews/rkdeveloptool/0f5a2e3be76ce1878d65b35fd197b454c6a9068b";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, rkdeveloptool-src }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (builtins.fromJSON (builtins.readFile ./src-tauri/tauri.conf.json)).version;
    in
    {
      packages = forAllSystems (pkgs: rec {
        # Multidevice fork of rkdeveloptool. Dynamic libusb is fine here —
        # the store pins the exact library, unlike a portable folder.
        rkdeveloptool = pkgs.stdenv.mkDerivation {
          pname = "rkdeveloptool-multidevice";
          version = "unstable-2026-08-13";
          src = rkdeveloptool-src;

          nativeBuildInputs = [ pkgs.autoreconfHook pkgs.pkg-config ];
          buildInputs = [ pkgs.libusb1 ];

          # Upstream Makefile.am uses -Werror; keep parity with the CI builds.
          env = {
            CFLAGS = "-Wno-unused-variable -Wno-error=unused-variable";
            CXXFLAGS = "-Wno-unused-variable -Wno-error=unused-variable";
          };

          meta = {
            description = "Rockchip flashing tool (multidevice fork)";
            homepage = "https://github.com/Anthony-Andrews/rkdeveloptool";
            license = pkgs.lib.licenses.gpl2Plus;
            platforms = pkgs.lib.platforms.linux;
            mainProgram = "rkdeveloptool";
          };
        };

        rockchip-universal-imager = pkgs.rustPlatform.buildRustPackage {
          pname = "rockchip-universal-imager";
          inherit version;
          src = self;

          cargoRoot = "src-tauri";
          buildAndTestSubdir = "src-tauri";
          cargoLock.lockFile = ./src-tauri/Cargo.lock;

          nativeBuildInputs = with pkgs; [
            cargo-tauri.hook
            pkg-config
            wrapGAppsHook3
            copyDesktopItems
          ];

          # Tauri v2 Linux stack. libusb comes vendored via rusb (static).
          buildInputs = with pkgs; [
            webkitgtk_4_1
            gtk3
            libsoup_3
            openssl
            glib-networking
          ];

          desktopItems = [
            (pkgs.makeDesktopItem {
              name = "rockchip-universal-imager";
              exec = "rockchip-universal-imager";
              icon = "rockchip-universal-imager";
              desktopName = "Rockchip Universal Imager";
              comment = "Rockchip flashing and eMMC helper";
              categories = [ "Utility" "Development" ];
            })
          ];

          postInstall = ''
            # Companions beside the executable — same layout as the portable
            # folder, found via executable_dir() in paths.rs.
            ln -s ${rkdeveloptool}/bin/rkdeveloptool $out/bin/rkdeveloptool
            cp -r ${./dependencies/loader_binaries} $out/bin/loader_binaries

            install -Dm644 ${./src-tauri/icons/128x128.png} \
              $out/share/icons/hicolor/128x128/apps/rockchip-universal-imager.png

            # USB access without root: NixOS users add this package to
            # services.udev.packages. 2207 = Rockchip vendor ID (maskrom/loader).
            install -Dm644 ${pkgs.writeText "70-rockchip-usb.rules" ''
              SUBSYSTEM=="usb", ATTRS{idVendor}=="2207", MODE="0660", TAG+="uaccess"
            ''} $out/lib/udev/rules.d/70-rockchip-usb.rules
          '';

          meta = {
            description = "Rockchip flashing and eMMC helper (Tauri GUI + rkdeveloptool)";
            homepage = "https://github.com/Anthony-Andrews/Rockchip-Universal-Imager";
            # Note: dependencies/loader_binaries contains redistributable
            # Rockchip loader blobs (see the rkbin repository license).
            platforms = pkgs.lib.platforms.linux;
            mainProgram = "rockchip-universal-imager";
          };
        };

        default = rockchip-universal-imager;
      });

      nixosModules = rec {
        rockchip-universal-imager = { config, lib, pkgs, ... }:
          let
            cfg = config.programs.rockchip-universal-imager;
            pkg = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          in
          {
            options.programs.rockchip-universal-imager.enable =
              lib.mkEnableOption "Rockchip Universal Imager (installs the app and the Rockchip USB udev rule)";
            config = lib.mkIf cfg.enable {
              environment.systemPackages = [ pkg ];
              # Grants seated users access to Rockchip USB devices (uaccess tag).
              services.udev.packages = [ pkg ];
              # System-level substituter for the prebuilt cache. The flake's
              # nixConfig prompt only works for trusted users (root-only by
              # default), which silently falls back to building from source;
              # declaring it here applies to every user on the machine.
              nix.settings.substituters = [ "https://antho.cachix.org" ];
              nix.settings.trusted-public-keys = [
                "antho.cachix.org-1:NU2YLmMqJD1O221PjtpZU0rBpzaoNesspvyQeBra0yc="
              ];
            };
          };
        default = rockchip-universal-imager;
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.system}.rockchip-universal-imager ];
          packages = with pkgs; [
            cargo-tauri
            rustc
            cargo
            # for hacking on the rkdeveloptool fork
            autoconf
            automake
            libtool
            libusb1
          ];
        };
      });
    };
}
