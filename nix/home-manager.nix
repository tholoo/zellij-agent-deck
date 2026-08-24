{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.zellij-agent-deck;
  defaultPackage = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
in
{
  options.programs.zellij-agent-deck = {
    enable = lib.mkEnableOption "the Zellij Agent Deck host bridge and plugin";

    package = lib.mkOption {
      type = lib.types.package;
      default = defaultPackage;
      defaultText = lib.literalExpression "inputs.zellij-agent-deck.packages.${pkgs.system}.default";
      description = "Zellij Agent Deck package to install.";
    };

    installPlugin = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Install the WASM plugin under the Zellij configuration directory.";
    };

    resurrection = {
      retentionDays = lib.mkOption {
        type = lib.types.ints.unsigned;
        default = 7;
        description = "Days to retain unreferenced Codex resurrection mappings.";
      };

      stateDirectory = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Optional override for the resurrection mapping directory.";
      };

      zellijCacheDirectory = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Optional override for the Zellij cache directory scanned by cleanup.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."zellij/plugins/agent-deck.wasm" = lib.mkIf cfg.installPlugin {
      source = "${cfg.package}/share/zellij/plugins/agent-deck.wasm";
    };

    home.sessionVariables = {
      ZELLIJ_AGENT_DECK_RESURRECTION_RETENTION_DAYS = toString cfg.resurrection.retentionDays;
    }
    // lib.optionalAttrs (cfg.resurrection.stateDirectory != null) {
      ZELLIJ_AGENT_DECK_RESURRECTION_DIR = cfg.resurrection.stateDirectory;
    }
    // lib.optionalAttrs (cfg.resurrection.zellijCacheDirectory != null) {
      ZELLIJ_AGENT_DECK_ZELLIJ_CACHE_DIR = cfg.resurrection.zellijCacheDirectory;
    };
  };
}
