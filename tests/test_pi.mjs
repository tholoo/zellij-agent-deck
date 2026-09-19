import assert from "node:assert/strict";
import { test } from "node:test";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { setup } from "../pi/extension.js";

const flush = () => new Promise((resolve) => setImmediate(resolve));
function fixture(options = {}) {
  const handlers = new Map();
  const sent = [];
  let poll;
  let stopped = 0;
  const session = {
    id: "11111111-1111-4111-8111-111111111111",
    file: "/tmp/session directory/exact-session.jsonl", name: "",
  };
  const ctx = {
    cwd: "/tmp/project", model: { provider: "test", id: "model" }, isIdle: () => true,
    sessionManager: {
      getSessionId: () => session.id, getSessionFile: () => session.file,
      getSessionName: () => session.name,
    },
  };
  setup({ on: (name, fn) => handlers.set(name, fn) }, {
    env: { ZELLIJ_SESSION_NAME: "dev", ZELLIJ_PANE_ID: "0", ...options.env },
    send: options.useBridge ? undefined : options.send ?? ((payload) => { sent.push(payload); }),
    setInterval: (fn) => { poll = fn; return 1; },
    clearInterval: () => { poll = undefined; stopped++; },
  });
  return {
    sent, ctx, session, handlers, stopped: () => stopped, poll: () => poll?.(),
    event: (name, event = {}) => handlers.get(name)?.({ type: name, ...event }, ctx),
  };
}

test("Pi tracks bounded lifecycle, tools, metadata and results without arguments or reasoning", async () => {
  const f = fixture();
  assert.equal(f.sent.length, 0, "factory must not start background work");
  f.event("session_start");
  f.event("before_agent_start", { prompt: "Implement Pi\n" + "x".repeat(200) });
  f.event("agent_start");
  f.event("tool_execution_start", { toolCallId: "t1", toolName: "bash", args: { command: "secret command" } });
  f.event("tool_execution_end", { toolCallId: "t1", toolName: "bash", isError: true, result: "private output" });
  f.event("agent_end", { messages: [{ role: "assistant", stopReason: "stop", content: [
    { type: "thinking", thinking: "private reasoning" }, { type: "text", text: "Done\n" + "a".repeat(300) },
  ] }] });
  f.session.name = "User-chosen name";
  f.poll();
  await flush();
  assert.equal(f.sent[0].event, "attach");
  assert.equal(f.sent[0].session_file, f.session.file);
  assert.match(f.sent[0].attachment_id, /^[a-f0-9]{32}$/);
  assert.ok(f.sent.some((p) => p.activity === "Using bash"));
  assert.ok(f.sent.some((p) => p.activity === "Failed bash"));
  const done = f.sent.find((p) => p.status === "done");
  assert.equal(done.message.length, 180);
  assert.match(done.attention_id, /^[a-f0-9]{32}$/);
  assert.equal(f.sent.at(-1).title, "User-chosen name");
  assert.equal(f.sent.at(-1).status, undefined, "metadata cannot overwrite completion");
  assert.doesNotMatch(JSON.stringify(f.sent), /secret command|private output|private reasoning/);
  await f.event("session_shutdown");
  assert.equal(f.sent.at(-1).event, "detach");
  assert.equal(f.stopped(), 1);
});

test("Pi reports question tools, errors and interruption with fresh result generations", async () => {
  const f = fixture();
  f.event("session_start");
  f.event("agent_start");
  f.event("tool_execution_start", { toolCallId: "q1", toolName: "ask_user_question" });
  f.event("tool_execution_start", { toolCallId: "t1", toolName: "read" });
  await flush();
  assert.equal(f.sent.at(-1).status, "needs_input");
  assert.equal(f.sent.at(-1).attention_id, "q1");
  f.event("tool_execution_end", { toolCallId: "q1", toolName: "ask_user_question" });
  f.event("agent_end", { messages: [{ role: "assistant", stopReason: "error", errorMessage: "Unavailable" }] });
  f.event("agent_start");
  f.event("agent_end", { messages: [{ role: "assistant", stopReason: "aborted" }] });
  f.event("agent_start");
  f.event("agent_end", { messages: [] });
  await f.event("session_shutdown");
  const failure = f.sent.find((p) => p.status === "error");
  const done = f.sent.find((p) => p.status === "done");
  assert.equal(failure.message, "Unavailable");
  assert.notEqual(failure.attention_id, done.attention_id);
  const interrupted = f.sent.find((p) => p.message === "Interrupted");
  assert.equal(interrupted.status, "idle");
  assert.equal(interrupted.attention_id, "");
});

test("Pi session switching, fork and reload detach old tokens and preserve queued identity", async () => {
  let release;
  const sent = [];
  const gate = new Promise((resolve) => { release = resolve; });
  const f = fixture({ send: async (p) => { await gate; sent.push(p); } });
  f.event("session_start");
  f.event("agent_start");
  const oldID = f.session.id;
  const shuttingDown = f.event("session_shutdown");
  f.session.id = "22222222-2222-4222-8222-222222222222";
  f.session.file = "/tmp/fork.jsonl";
  f.event("session_start", { reason: "fork" });
  f.event("session_start", { reason: "reload" });
  release();
  await shuttingDown;
  await f.event("session_shutdown");
  const attachments = sent.filter((p) => p.event === "attach");
  assert.equal(attachments.length, 3);
  assert.equal(new Set(attachments.map((p) => p.attachment_id)).size, 3);
  const firstDetach = sent.findIndex((p) => p.event === "detach");
  assert.ok(sent.slice(0, firstDetach + 1).every((p) => p.session_id === oldID));
  assert.equal(attachments[1].session_file, "/tmp/fork.jsonl");
  assert.equal(f.stopped(), 3);
});

test("aborting during a tool does not announce the preceding tool-call message as a result", async () => {
  const f = fixture();
  f.event("session_start");
  f.event("agent_start");
  f.ctx.signal = AbortSignal.abort();
  f.event("agent_end", { messages: [{ role: "assistant", stopReason: "toolUse", content: [] }] });
  await flush();
  assert.equal(f.sent.at(-1).status, "idle");
  assert.equal(f.sent.at(-1).message, "Interrupted");
  assert.equal(f.sent.at(-1).attention_id, "");
  await f.event("session_shutdown");
});

test("Pi ignores non-Zellij processes and handles ephemeral sessions and unavailable helper", async () => {
  for (const env of [{ ZELLIJ_SESSION_NAME: "" }, { ZELLIJ_PANE_ID: "bad" }]) {
    const f = fixture({ env });
    assert.equal(f.handlers.size, 0);
  }
  const f = fixture();
  f.session.file = undefined;
  f.event("session_start");
  await f.event("session_shutdown");
  assert.equal(f.sent[0].session_file, "");
  const unavailable = fixture({ useBridge: true, env: { ZELLIJ_AGENT_DECK_COMMAND: "/missing/deck-helper" } });
  unavailable.event("session_start");
  await unavailable.event("session_shutdown");
});

test("Pi transport sends JSON on stdin without invoking a shell", async () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "deck-pi-"));
  try {
    const output = path.join(root, "payloads");
    const helper = path.join(root, "helper with spaces");
    writeFileSync(helper, `#!${process.execPath}\nimport('node:fs').then(fs => {
      let data=''; process.stdin.on('data', chunk => data+=chunk);
      process.stdin.on('end', () => fs.appendFileSync(${JSON.stringify(output)}, JSON.stringify({args:process.argv.slice(2), payload:JSON.parse(data)})+'\\n'));
    });\n`);
    chmodSync(helper, 0o700);
    const f = fixture({ useBridge: true, env: { ZELLIJ_AGENT_DECK_COMMAND: helper } });
    f.session.name = "literal $(touch SHOULD_NOT_EXIST) `echo nope`";
    f.event("session_start");
    await f.event("session_shutdown");
    const rows = readFileSync(output, "utf8").trim().split("\n").map(JSON.parse);
    assert.ok(rows.every((r) => JSON.stringify(r.args) === '["pi-hook"]'));
    assert.equal(rows[0].payload.title, f.session.name);
    assert.equal(rows.at(-1).payload.event, "detach");
  } finally { rmSync(root, { recursive: true, force: true }); }
});
