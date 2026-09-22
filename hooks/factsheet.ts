import type { EngineInterface, Register } from "claude-code";

// The engine's data dir for a plugin, what the command hook gets as CLAUDE_PLUGIN_DATA:
// <config>/plugins/data/<name>-<marketplace>; "inline" for a --plugin-dir plugin.
const dataDirOf = async ($: EngineInterface): Promise<string> => {
  const root = $.plugin.root;
  const cached = root.match(/^(.*\/plugins)\/cache\/([^/]+)\/([^/]+)\//);
  if (cached !== null) return `${cached[1]}/data/${cached[3]}-${cached[2]}`;
  const configDir = (await $.env.get("CLAUDE_CONFIG_DIR")) ?? `${await $.env.get("HOME")}/.claude`;
  return `${configDir}/plugins/data/${$.plugin.name}-inline`;
};

export const register: Register = (on) => {
  // Answering without next() keeps the SessionStart command hook below from running: one injection, not two.
  on("classic.SessionStart", async ($, e, next) => {
    const root = $.plugin.root;
    const { exitCode, stdout } = await $.process.run(["bash", `${root}/hooks/session-start.sh`], {
      cwd: e.cwd,
      env: { CLAUDE_PLUGIN_ROOT: root, CLAUDE_PLUGIN_DATA: await dataDirOf($) },
      timeoutMs: 60_000,
    });
    if (exitCode !== 0 || stdout.trim() === "") return next(e);
    return { additionalContext: [stdout.trim()] };
  });
};
