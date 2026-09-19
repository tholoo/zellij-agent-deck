import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";

const clean = (value, limit) => String(value ?? "")
  .replace(/[\x00-\x1f\x7f]/g, " ").replace(/\s+/g, " ").trim().slice(0, limit);
const token = () => randomBytes(16).toString("hex");
const questionTools = new Set(["ask_question", "ask_user_question", "question", "questionnaire"]);

function send(payload, command) {
  // Pi's standalone executable uses Bun. Feed a Blob directly: its Node
  // compatibility stream can close stdin before delivering the payload.
  if (typeof Bun !== "undefined") {
    const child = Bun.spawn([command, "pi-hook"], {
      stdin: new Blob([JSON.stringify(payload)]), stdout: "ignore", stderr: "ignore",
    });
    const timer = setTimeout(() => child.kill("SIGKILL"), 4000);
    return child.exited.finally(() => clearTimeout(timer));
  }
  return new Promise((resolve) => {
    const child = spawn(command, ["pi-hook"], { stdio: ["pipe", "ignore", "ignore"] });
    const timer = setTimeout(() => child.kill("SIGKILL"), 4000);
    const finish = () => { clearTimeout(timer); resolve(); };
    child.on("error", finish);
    child.on("close", finish);
    child.stdin.on("error", () => {});
    child.stdin.end(JSON.stringify(payload));
  });
}

// Pi loads index.ts through its extension loader; this dependency-free module
// also runs under Node for tests. No background work starts in the factory.
export function setup(pi, runtime = {}) {
  const env = runtime.env ?? process.env;
  if (!env.ZELLIJ_SESSION_NAME || !/^\d+$/.test(env.ZELLIJ_PANE_ID ?? "")) return;
  const transmit = runtime.send ?? ((payload) => send(
    payload, env.ZELLIJ_AGENT_DECK_COMMAND || "zellij-agent-deck",
  ));
  const every = runtime.setInterval ?? setInterval;
  const cancel = runtime.clearInterval ?? clearInterval;
  let active;
  let queue = Promise.resolve();
  let timer;
  let metadata = "";
  let fallbackTitle = "Pi session";
  let resultID;
  const questions = new Map();

  const emit = (event, fields = {}) => {
    if (!active) return;
    const payload = { ...active, event, ...fields };
    // Capture identity now: a queued update must never attach to a later session.
    queue = queue.then(() => transmit(payload)).catch(() => {});
  };
  const fields = (ctx) => ({
    cwd: ctx.cwd,
    session_file: ctx.sessionManager.getSessionFile() ?? "",
    title: clean(ctx.sessionManager.getSessionName() || fallbackTitle, 72),
    model: clean(ctx.model ? `${ctx.model.provider}/${ctx.model.id}` : "", 64),
  });
  const refresh = (ctx) => {
    if (!active || active.session_id !== ctx.sessionManager.getSessionId()) return;
    const snapshot = fields(ctx);
    const signature = JSON.stringify(snapshot);
    if (signature !== metadata) {
      metadata = signature;
      emit("update", snapshot);
    }
  };
  const state = (status, message = "", activity = "", attentionID = "") => {
    emit("update", {
      status, message: clean(message, 180), activity: clean(activity, 180),
      attention_id: attentionID,
    });
  };
  const detach = () => {
    if (timer !== undefined) cancel(timer);
    timer = undefined;
    emit("detach");
    active = undefined;
    questions.clear();
  };
  pi.on("session_start", (_event, ctx) => {
    detach();
    active = { session_id: ctx.sessionManager.getSessionId(), attachment_id: token() };
    metadata = "";
    fallbackTitle = "Pi session";
    resultID = undefined;
    emit("attach", { ...fields(ctx), status: ctx.isIdle() ? "idle" : "working" });
    refresh(ctx);
    // /name and session-file persistence do not emit metadata events in 0.85.
    timer = every(() => refresh(ctx), 500);
    timer?.unref?.();
  });
  pi.on("before_agent_start", (event, ctx) => {
    if (fallbackTitle === "Pi session") fallbackTitle = clean(event.prompt, 72) || fallbackTitle;
    refresh(ctx);
  });
  pi.on("agent_start", (_event, ctx) => {
    resultID = token();
    questions.clear();
    refresh(ctx);
    state("working", "", "Working");
  });
  pi.on("model_select", (_event, ctx) => refresh(ctx));
  pi.on("tool_execution_start", (event) => {
    if (questionTools.has(event.toolName)) {
      questions.set(event.toolCallId, event.toolName);
      state("needs_input", `Question: ${event.toolName}`, "", event.toolCallId);
    } else if (!questions.size) {
      state("working", "", `Using ${event.toolName}`);
    }
  });
  pi.on("tool_execution_end", (event) => {
    questions.delete(event.toolCallId);
    if (!questions.size) {
      state("working", "", `${event.isError ? "Failed" : "Finished"} ${event.toolName}`);
    }
  });
  pi.on("agent_end", (event, ctx) => {
    refresh(ctx);
    questions.clear();
    const last = event.messages?.findLast((message) => message.role === "assistant");
    if (ctx.signal?.aborted || last?.stopReason === "aborted") {
      state("idle", "Interrupted");
    } else if (last?.stopReason === "error") {
      state("error", last.errorMessage || "Session failed", "", resultID || token());
    } else {
      const excerpt = (last?.content ?? []).filter((part) => part.type === "text")
        .map((part) => clean(part.text, 180)).join(" ");
      state("done", excerpt || "Session completed", "", resultID || token());
    }
  });
  pi.on("session_shutdown", async () => {
    detach();
    await queue;
  });
}

export default function agentDeck(pi) { setup(pi); }
