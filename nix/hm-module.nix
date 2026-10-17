# home-manager module: services.ukiyo.
#
#   imports = [ ukiyo.homeManagerModules.default ];
#   services.ukiyo = {
#     enable = true;
#     settings = {
#       scene = "kabukicho-gate";
#       performance.idle_timeout = 600;
#     };
#   };
#
# `settings` becomes $XDG_CONFIG_HOME/ukiyo/defaults.toml, the declarative
# layer. `ukiyo config set` writes config.toml next to it, which wins, so a
# settings UI keeps working on top of a read-only defaults file.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.ukiyo;
  toml = pkgs.formats.toml { };
in
{
  options.services.ukiyo = {
    enable = lib.mkEnableOption "ukiyo, reactive live wallpapers for Wayland";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "ukiyo.packages.\${system}.default";
      description = "The ukiyo package.";
    };

    settings = lib.mkOption {
      inherit (toml) type;
      default = { };
      example = lib.literalExpression ''
        {
          scene = "kabukicho-gate";
          performance.on_battery = "reduced";
          scenes.kabukicho-gate.rain = 0.6;
        }
      '';
      description = ''
        Written to {file}`$XDG_CONFIG_HOME/ukiyo/defaults.toml`. Every key
        is listed by `ukiyo config schema`. The user's own
        {file}`config.toml` (what `ukiyo config set` writes) overrides it.
      '';
    };

    systemdTarget = lib.mkOption {
      type = lib.types.str;
      default = "graphical-session.target";
      description = "The systemd user target the daemon is started with and bound to.";
    };

    enableZshIntegration = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Source `ukiyo shell-hook zsh` (generated at build time) in
        interactive zsh, so finished builds pulse the wallpaper.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."ukiyo/defaults.toml" = lib.mkIf (cfg.settings != { }) {
      source = toml.generate "ukiyo-defaults.toml" cfg.settings;
    };

    systemd.user.services.ukiyo = {
      Unit = {
        Description = "ukiyo live wallpaper";
        Documentation = [ "https://github.com/SuplenC/ukiyo" ];
        PartOf = [ cfg.systemdTarget ];
        After = [ cfg.systemdTarget ];
        ConditionEnvironment = "WAYLAND_DISPLAY";
        X-Restart-Triggers = lib.optional (cfg.settings != { }) (
          toml.generate "ukiyo-defaults.toml" cfg.settings
        );
      };
      Service = {
        ExecStart = "${lib.getExe cfg.package} daemon";
        Restart = "on-failure";
        RestartSec = 2;
        # The bundled scenes, plus any installed in the user's Nix profile.
        Environment = [
          "XDG_DATA_DIRS=%h/.nix-profile/share:${cfg.package}/share:/usr/local/share:/usr/share"
        ];
      };
      Install.WantedBy = [ cfg.systemdTarget ];
    };

    programs.zsh.initContent = lib.mkIf cfg.enableZshIntegration (
      lib.mkOrder 1100 ''
        source ${
          pkgs.runCommand "ukiyo-hook.zsh" { } ''
            ${lib.getExe cfg.package} shell-hook zsh >$out
          ''
        }
      ''
    );
  };
}
