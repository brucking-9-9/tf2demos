# home-manager module: installs tf2demos, generates ~/.config/tf2demos/config.toml,
# and runs `tf2demos organize` from a daily systemd user timer.
#
# Wrapped by flake.nix as `homeManagerModules.default = import ./nix/hm-module.nix { inherit self; }`
# so that `package` can default to this flake's own build.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.tf2demos;
  settingsFormat = pkgs.formats.toml { };
in
{
  options.services.tf2demos = {
    enable = lib.mkEnableOption "tf2demos daily demo organizer (systemd user timer)";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "tf2demos.packages.\${system}.default";
      description = "The tf2demos package to install and run from the timer.";
    };

    settings = lib.mkOption {
      description = ''
        Contents of {file}`~/.config/tf2demos/config.toml`.
        The app never writes this file. Each key is a separate option, so a
        single key can be overridden while the others keep their defaults;
        extra keys are passed through verbatim.
      '';
      default = { };
      type = lib.types.submodule {
        freeformType = settingsFormat.type;
        options = {
          tf_dir = lib.mkOption {
            type = lib.types.str;
            default = "/home/brucking/.local/share/Steam/steamapps/common/Team Fortress 2/tf";
            description = "The TF2 `tf/` directory; demos are read from `<tf_dir>/demos`.";
          };
          archive_dir = lib.mkOption {
            type = lib.types.str;
            default = "demos/archive";
            description = "Archive location, relative to `tf_dir` so `playdemo demos/archive/...` works in-game.";
          };
          age_hours = lib.mkOption {
            type = lib.types.numbers.nonnegative;
            default = 24;
            description = "A demo is archived only once its mtime is older than this many hours.";
          };
          group_secs = lib.mkOption {
            type = lib.types.numbers.nonnegative;
            default = 2.0;
            description = "Sidecar marks within this many seconds of each other are merged into one event.";
          };
          seed_labels = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [
              "surf stab"
              "c-tap"
              "matador"
            ];
            description = "Labels seeded into index.json when it is first created.";
          };
          classes = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [
              "scout"
              "soldier"
              "pyro"
              "demoman"
              "heavy"
              "engineer"
              "medic"
              "sniper"
              "spy"
            ];
            description = "Player classes offered by the labelling wizard.";
          };
        };
      };
    };

    onCalendar = lib.mkOption {
      type = lib.types.str;
      default = "daily";
      example = "03:30";
      description = "systemd `OnCalendar` expression for the organize timer.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."tf2demos/config.toml".source =
      settingsFormat.generate "tf2demos-config.toml" cfg.settings;

    systemd.user.services.tf2demos-organize = {
      Unit.Description = "tf2demos: archive day-old TF2 demos";
      Service = {
        Type = "oneshot";
        ExecStart = "${lib.getExe cfg.package} organize";
        Nice = 10;
        IOSchedulingClass = "idle";
        # No PATH/Wayland/DBus environment needed: the TF2-running check reads /proc directly.
      };
    };

    systemd.user.timers.tf2demos-organize = {
      Unit.Description = "Daily tf2demos organize";
      Timer = {
        OnCalendar = cfg.onCalendar;
        Persistent = true;
        RandomizedDelaySec = "10min";
      };
      Install.WantedBy = [ "timers.target" ];
    };
  };
}
