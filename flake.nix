{
  description = "ukiyo: reactive live wallpapers for Wayland";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        ukiyo = pkgs.callPackage ./nix/package.nix { };
        default = ukiyo;
      });

      overlays.default = final: _prev: {
        ukiyo = final.callPackage ./nix/package.nix { };
      };

      # `services.ukiyo` for home-manager: the package, a systemd user
      # service bound to the graphical session, and defaults.toml.
      homeManagerModules.default = import ./nix/hm-module.nix self;

      checks = forAllSystems (pkgs: {
        ukiyo = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-tree);
    };
}
