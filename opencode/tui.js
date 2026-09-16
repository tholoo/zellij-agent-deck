import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import path from "node:path";

const clean = (value, limit) => String(value ?? "")
  .replace(/[\x00-\x1f\x7f]/g, " ").replace(/\s+/g, " ").trim().slice(0, limit);

// Payloads travel over stdin, never through a shell or command-line arguments.
function send(payload, command) {
  return new Promise((resolve) => {
    const child = spawn(command, ["opencode-hook"], { stdio: ["pipe", "ignore", "ignore"] });
    const timer = setTimeout(() => child.kill("SIGKILL"), 4000);
    const finish = () => { clearTimeout(timer); resolve(); };
    child.on("error", finish);
    child.on("close", finish);
    child.stdin.on("error", () => {});
    child.stdin.end(JSON.stringify(payload));
  });
}

// The definition is the plain object accepted by OpenCode 2's Plugin.define.
// Keeping it dependency-free also lets the adapter run under Node in tests.
export function setup(context, runtime = {}) {
  const env = runtime.env ?? process.env;
  if (!env.ZELLIJ_SESSION_NAME || !/^\d+$/.test(env.ZELLIJ_PANE_ID ?? "")) return;
  const transmit = runtime.send ?? ((payload) => send(
    payload, env.ZELLIJ_AGENT_DECK_COMMAND || "zellij-agent-deck",
  ));
  const every = runtime.setInterval ?? setInterval;
  const cancel = runtime.clearInterval ?? clearInterval;
  let active;
  let deletedSession;
  let queue = Promise.resolve();
  let metadata = "";
  let excerpt = "";
  let tools = new Map();
  let permissions = new Map();
  let forms = new Map();

  const emit = (event, fields = {}) => {
    if (!active) return;
    const payload = { session_id: active.session_id, attachment_id: active.attachment_id, event, ...fields };
    queue = queue.then(() => transmit(payload)).catch(() => {});
  };
  const state = (status, message = "", activity = "", attentionID = "") => {
    emit("update", {
      status, message: clean(message, 180), activity: clean(activity, 180),
      attention_id: clean(attentionID, 128),
    });
  };
  const pending = () => {
    const permission = permissions.values().next().value;
    const form = forms.values().next().value;
    if (permission) {
      state("needs_input", `Approval: ${permission.message || permission.action}`, "", permission.id);
    } else if (form) {
      state("needs_input", `Question: ${form.title}`, "", form.id);
    } else {
      state("working", "", "Working");
    }
  };
  const poll = () => {
    const route = context.ui.router.current();
    if (route.type !== "session" || route.sessionID !== deletedSession) deletedSession = undefined;
    if (route.type === "session" && route.sessionID === deletedSession) return;
    const session = route.type === "session" ? context.data.session.get(route.sessionID) : undefined;
    // A loading session is not a route change. Wait for its cached metadata.
    if (route.type === "session" && !session) return;
    if (active?.session_id !== session?.id) {
      emit("detach");
      active = undefined;
      metadata = "";
      excerpt = "";
      tools = new Map();
      permissions = new Map();
      forms = new Map();
      if (session) {
        active = { session_id: session.id, attachment_id: randomBytes(16).toString("hex") };
        permissions = new Map((context.data.session.permission.list(session.id) ?? []).map((p) => [p.id, p]));
        forms = new Map((context.data.session.form.list(session.id) ?? []).map((f) => [f.id, f]));
        emit("attach", {
          cwd: path.resolve(session.location.directory, session.subpath || "."),
          title: clean(session.title || "OpenCode session", 72),
        });
        if (permissions.size || forms.size) pending();
        else if (context.data.session.status(session.id) === "running") state("working", "", "Working");
        else if (session.outcome === "failed") state("error", "Session failed", "", `failed:${session.time.idle}`);
        else if (session.outcome === "interrupted") state("idle", "Interrupted");
        // Opening old history should not announce a new result.
        else if (session.outcome === "succeeded") state("done", "Session completed");
        else state("idle");
      }
    }
    if (!active || !session) return;
    const fields = {
      cwd: path.resolve(session.location.directory, session.subpath || "."),
      title: clean(session.title || "OpenCode session", 72),
      model: clean(session.model ? `${session.model.providerID}/${session.model.id}` : "", 64),
    };
    const signature = JSON.stringify(fields);
    if (signature !== metadata) {
      metadata = signature;
      emit("update", fields);
    }
  };

  const stop = context.data.listen(({ details: event }) => {
    poll();
    const data = event.data;
    if (!active || (data?.sessionID ?? data?.form?.sessionID) !== active.session_id) return;
    switch (event.type) {
      case "session.execution.started":
        excerpt = "";
        tools.clear();
        state("working", "", "Working");
        break;
      case "session.execution.succeeded":
        state("done", excerpt || "Session completed", "", event.id || randomBytes(16).toString("hex"));
        break;
      case "session.execution.failed":
        state("error", data.error?.message || "Session failed", "", event.id || randomBytes(16).toString("hex"));
        break;
      case "session.execution.interrupted":
        permissions.clear();
        forms.clear();
        state("idle", "Interrupted");
        break;
      case "permission.asked":
        permissions.set(data.id, { id: data.id, message: clean(data.message, 160), action: clean(data.action, 160) });
        pending();
        break;
      case "permission.replied":
        permissions.delete(data.requestID);
        pending();
        break;
      case "form.created":
        forms.set(data.form.id, { id: data.form.id, title: clean(data.form.title, 160) });
        pending();
        break;
      case "form.replied":
      case "form.cancelled":
        forms.delete(data.id);
        pending();
        break;
      case "session.tool.input.started":
        tools.set(data.id, clean(data.name, 100));
        if (!permissions.size && !forms.size) state("working", "", `Using ${clean(data.name, 100)}`);
        break;
      case "session.tool.success":
      case "session.tool.failed": {
        const name = tools.get(data.id) || "tool";
        tools.delete(data.id);
        if (!permissions.size && !forms.size) {
          state("working", "", `${event.type.endsWith("failed") ? "Failed" : "Finished"} ${name}`);
        }
        break;
      }
      case "session.text.ended":
        excerpt = clean(data.text, 180);
        break;
      case "session.deleted":
        deletedSession = active.session_id;
        emit("delete");
        active = undefined;
        break;
    }
  });
  poll();
  // Route changes are local UI state, not server events. No network polling.
  const timer = every(poll, 500);
  timer?.unref?.();
  return async () => {
    cancel(timer);
    stop();
    emit("detach");
    active = undefined;
    await queue;
  };
}

export default { id: "zellij.agent-deck", setup: (context) => setup(context) };
