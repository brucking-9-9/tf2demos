# home-manager module: installs tf2demos, generates ~/.config/tf2demos/{config,theme}.toml,
# runs `tf2demos organize` from a daily systemd user timer, and `tf2demos watch` as a
# graphical-session service that prompts for labelling when TF2 exits.
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
  # HANDOFF §2 palette ("cyberpunk-neon"); the app compiles the same defaults in.
  defaultTheme = {
    bg = "#120B10";
    panel = "#563466";
    dim = "#6D5A78";
    text = "#EEFFFF";
    cyan = "#00FFC8";
    pink = "#FF0055";
    purple = "#D57BFF";
    yellow = "#F4EF00";
    green = "#00FF9C";
    blue = "#76C1FF";
  };
  hexColor = lib.types.strMatching "#[0-9a-fA-F]{6}";
in
{
  options.services.tf2demos = {
    enable = lib.mkEnableOption "tf2demos: daily demo organizer timer, TF2-exit watcher, review wizard";

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
          poll_secs = lib.mkOption {
            type = lib.types.ints.positive;
            default = 5;
            description = "How often `tf2demos watch` checks whether TF2 is running.";
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

    watcher.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Run `tf2demos watch` as a user service bound to `graphical-session.target`.
        On TF2 exit it counts unlabelled marks and shows a mako notification whose
        *Review* action opens the labelling wizard. It never moves or deletes files.
      '';
    };

    theme = lib.mkOption {
      type = lib.types.submodule {
        options = lib.mapAttrs (
          name: default:
          lib.mkOption {
            type = hexColor;
            inherit default;
            description = "Palette entry `${name}` of the review wizard (`#RRGGBB`).";
          }
        ) defaultTheme;
      };
      default = { };
      description = ''
        Contents of {file}`~/.config/tf2demos/theme.toml`, read by the review wizard.
        Defaults to the rice palette; the app never writes this file.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."tf2demos/config.toml".source =
      settingsFormat.generate "tf2demos-config.toml" cfg.settings;
    xdg.configFile."tf2demos/theme.toml".source =
      settingsFormat.generate "tf2demos-theme.toml" cfg.theme;

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

    systemd.user.services.tf2demos-watch = lib.mkIf cfg.watcher.enable {
      Unit = {
        Description = "tf2demos: prompt to label marks when TF2 exits";
        PartOf = [ "graphical-session.target" ];
        After = [ "graphical-session.target" ];
      };
      Service = {
        Type = "simple";
        ExecStart = "${lib.getExe cfg.package} watch";
        Restart = "on-failure";
        RestartSec = 10;
        Nice = 10;
        # notify-send / wl-copy for the prompt and play-at-tick; `steam` for launching TF2 is
        # taken from the system profile rather than pulled into this closure. WAYLAND_DISPLAY and
        # DBUS_SESSION_BUS_ADDRESS reach user units from the graphical session (verified with
        # `systemctl --user show-environment`).
        Environment = [
          "PATH=${
            lib.makeBinPath [
              pkgs.libnotify
              pkgs.wl-clipboard
            ]
          }:/etc/profiles/per-user/${config.home.username}/bin:/run/current-system/sw/bin"
        ];
      };
      Install.WantedBy = [ "graphical-session.target" ];
    };
  };
}
