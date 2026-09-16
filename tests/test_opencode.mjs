import assert from "node:assert/strict";
import { test } from "node:test";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { setup } from "../opencode/tui.js";

const flush = () => new Promise((resolve) => setImmediate(resolve));

function fixture(options = {}) {
  const sessions = new Map(["ses_one", "ses_two"].map((id) => [id, {
    id, title: `Session ${id}`, location: { directory: "/tmp/project" },
    model: { providerID: "provider", id: "model" }, time: { idle: 123 },
  }]));
  let route = { type: "session", sessionID: "ses_one" };
  let listener;
  let poll;
  let stopped = false;
  const sent = [];
  const context = {
    ui: { router: { current: () => route } },
    data: {
      listen: (fn) => { listener = fn; return () => { stopped = true; }; },
      session: {
        get: (id) => sessions.get(id), status: () => "idle",
        permission: { list: () => options.permissions ?? [] },
        form: { list: () => [] },
      },
    },
  };
  const cleanup = setup(context, {
    env: { ZELLIJ_SESSION_NAME: "dev", ZELLIJ_PANE_ID: "0", ...options.env },
    send: options.useBridge ? undefined : options.send ?? ((payload) => { sent.push(payload); }),
    setInterval: (fn) => { poll = fn; },
    clearInterval: () => {},
  });
  return {
    sent, sessions, cleanup, stopped: () => stopped,
    poll: () => poll?.(),
    route: (next) => { route = next; poll(); },
    event: (type, data = {}, id = type) => listener({ details: {
      type, id, data: { sessionID: "ses_one", ...data },
    } }),
  };
}

test("OpenCode lifecycle reports bounded status without transcripts or tool arguments", async () => {
  const f = fixture();
  f.event("session.execution.started");
  f.event("session.tool.input.started", { id: "tool1", name: "bash" });
  f.event("session.tool.called", { id: "tool1", input: { command: "secret argument" } });
  f.event("permission.asked", { id: "per_one", action: "bash" });
  f.event("permission.replied", { requestID: "per_one" });
  f.event("session.tool.success", { id: "tool1", content: [{ text: "secret output" }] });
  f.event("session.text.ended", { text: "Finished\n" + "x".repeat(500) });
  f.event("session.execution.succeeded");
  await flush();
  assert.equal(f.sent[0].event, "attach");
  assert.equal(f.sent[0].session_id, "ses_one");
  assert.match(f.sent[0].attachment_id, /^[a-f0-9]{32}$/);
  assert.ok(f.sent.some((p) => p.status === "needs_input" && p.attention_id === "per_one"));
  assert.ok(f.sent.some((p) => p.activity === "Finished bash"));
  assert.equal(f.sent.at(-1).status, "done");
  assert.equal(f.sent.at(-1).message.length, 180);
  assert.doesNotMatch(JSON.stringify(f.sent), /secret|\\n/);
  await f.cleanup();
  assert.equal(f.sent.at(-1).event, "detach");
  assert.ok(f.stopped());
});

test("shared-server events never claim another TUI's session", async () => {
  const f = fixture();
  await flush();
  const count = f.sent.length;
  f.event("session.execution.succeeded", { sessionID: "ses_two" });
  f.poll();
  await flush();
  assert.equal(f.sent.length, count);
  f.route({ type: "session", sessionID: "ses_two" });
  await flush();
  const old = f.sent.find((p) => p.event === "detach");
  const current = f.sent.find((p) => p.event === "attach" && p.session_id === "ses_two");
  assert.equal(old.session_id, "ses_one");
  assert.notEqual(old.attachment_id, current.attachment_id);
  f.route({ type: "home" });
  await flush();
  assert.equal(f.sent.at(-1).event, "detach");
  assert.equal(f.sent.at(-1).session_id, "ses_two");
  await f.cleanup();
});

test("multiple approvals and forms stay pending until all are answered", async () => {
  const f = fixture({ permissions: [{ id: "per_first", action: "read" }] });
  f.event("form.created", { form: { id: "form_one", sessionID: "ses_one", title: "Which branch?" } });
  f.event("permission.replied", { requestID: "per_first" });
  await flush();
  assert.equal(f.sent.at(-1).status, "needs_input");
  assert.equal(f.sent.at(-1).message, "Question: Which branch?");
  f.event("form.cancelled", { id: "form_one" });
  await flush();
  assert.equal(f.sent.at(-1).status, "working");
  f.event("session.execution.failed", { error: { message: "Provider failed" } });
  f.poll();
  await flush();
  assert.equal(f.sent.at(-1).status, "error");
  f.event("session.execution.interrupted");
  await flush();
  assert.equal(f.sent.at(-1).status, "idle");
  assert.equal(f.sent.at(-1).message, "Interrupted");
  await f.cleanup();
});

test("metadata updates preserve terminal state and loading routes do not detach", async () => {
  const f = fixture();
  f.event("session.execution.succeeded");
  f.sessions.get("ses_one").title = "Renamed\n" + "x".repeat(100);
  f.poll();
  await flush();
  assert.equal(f.sent.at(-1).title.length, 72);
  assert.equal(f.sent.at(-1).status, undefined);
  const count = f.sent.length;
  f.route({ type: "session", sessionID: "ses_loading" });
  await flush();
  assert.equal(f.sent.length, count);
  await f.cleanup();
});

test("missing bridge cannot reject OpenCode hooks or poison later sends", async () => {
  const attempts = [];
  const f = fixture({ send: async (p) => { attempts.push(p); throw new Error("missing bridge"); } });
  f.event("session.execution.started");
  f.event("session.execution.succeeded");
  await f.cleanup();
  assert.equal(attempts.at(-1).event, "detach");
  assert.ok(attempts.some((p) => p.status === "done"));
});

test("plugin is inactive outside Zellij", () => {
  const f = fixture({ env: { ZELLIJ_SESSION_NAME: "" } });
  assert.equal(f.cleanup, undefined);
  assert.equal(f.sent.length, 0);
});

test("deleted sessions are not reattached while the UI cache still contains them", async () => {
  const f = fixture();
  f.event("session.deleted");
  f.poll();
  f.poll();
  await flush();
  assert.equal(f.sent.at(-1).event, "delete");
  assert.equal(f.sent.filter((p) => p.event === "attach").length, 1);
  await f.cleanup();
});

test("real bridge transport sends JSON over stdin in order, without shell expansion", async () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "agent-deck-bridge-"));
  try {
    const command = path.join(root, "bridge with spaces");
    const log = path.join(root, "events.jsonl");
    writeFileSync(command, `#!${process.execPath}
const fs = require("node:fs");
const payload = JSON.parse(fs.readFileSync(0, "utf8"));
fs.appendFileSync(${JSON.stringify(log)}, JSON.stringify({ args: process.argv.slice(2), payload }) + "\\n");
`);
    chmodSync(command, 0o700);
    const f = fixture({ useBridge: true, env: { ZELLIJ_AGENT_DECK_COMMAND: command } });
    f.sessions.get("ses_one").title = "Literal $(command) ; text";
    f.poll();
    f.event("session.execution.succeeded");
    await f.cleanup();
    const rows = readFileSync(log, "utf8").trim().split("\n").map(JSON.parse);
    assert.ok(rows.every((row) => row.args.join() === "opencode-hook"));
    assert.equal(rows[0].payload.event, "attach");
    assert.ok(rows.some((row) => row.payload.title === "Literal $(command) ; text"));
    assert.equal(rows.at(-2).payload.status, "done");
    assert.equal(rows.at(-1).payload.event, "detach");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
