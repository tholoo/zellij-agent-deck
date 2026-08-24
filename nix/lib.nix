{ lib, self }:
let
  example = builtins.fromJSON (builtins.readFile ../examples/hooks.json);
  manifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);

  replaceHook =
    agentDeckCommand: resurrectionCommand: hook:
    if (hook.command or null) == "zellij-agent-deck hook" then
      hook // { command = "${agentDeckCommand} hook"; }
    else if (hook.command or null) == "zellij-agent-deck-codex hook" then
      hook // { command = "${resurrectionCommand} hook"; }
    else
      hook;

  replaceGroup =
    agentDeckCommand: resurrectionCommand: group:
    group
    // {
      hooks = map (replaceHook agentDeckCommand resurrectionCommand) group.hooks;
    };
in
rec {
  requiredZellijVersion = lib.removePrefix "=" manifest.dependencies."zellij-tile";

  # examples/hooks.json owns the event topology. Callers only supply executable
  # locations, which keeps managed and user hooks in sync with the example.
  mkCodexHooks =
    {
      agentDeckCommand ? "zellij-agent-deck",
      resurrectionCommand ? "zellij-agent-deck-codex",
      includePostToolUse ? true,
    }:
    let
      hooks = lib.mapAttrs (
        _name: groups: map (replaceGroup agentDeckCommand resurrectionCommand) groups
      ) example.hooks;
    in
    if includePostToolUse then hooks else removeAttrs hooks [ "PostToolUse" ];

  mkCodexRequirements =
    {
      managedDir ? null,
      ...
    }@args:
    {
      features.hooks = true;
      hooks =
        mkCodexHooks (removeAttrs args [ "managedDir" ])
        // lib.optionalAttrs (managedDir != null) { managed_dir = managedDir; };
    };

  mkCodexWrapper =
    {
      pkgs,
      codex,
      agentDeck ? self.packages.${pkgs.stdenv.hostPlatform.system}.default,
      name ? "${codex.pname or "codex"}-zellij-agent-deck",
    }:
    let
      launcher = pkgs.writeShellApplication {
        name = "codex";
        text = ''
          exec ${lib.escapeShellArg "${agentDeck}/bin/zellij-agent-deck-codex"} codex-supervisor \
            --codex ${lib.escapeShellArg (lib.getExe codex)} -- "$@"
        '';
      };
    in
    pkgs.symlinkJoin {
      inherit name;
      paths = [ codex ];
      postBuild = ''
        rm -f "$out/bin/codex"
        ln -s ${lib.getExe launcher} "$out/bin/codex"
      '';
    };
}
