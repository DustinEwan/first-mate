<script setup lang="ts">
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ref, onMounted, watch, nextTick } from "vue";
import { NConfigProvider, NInput, NPopover, darkTheme } from "naive-ui";
import { DynamicScroller, DynamicScrollerItem } from "vue-virtual-scroller";
import { Marked } from "marked";
import { markedHighlight } from "marked-highlight";
import markedKatex from "marked-katex-extension";
import hljs from "highlight.js/lib/common";
import DOMPurify from "dompurify";
import SetupWizard from "./SetupWizard.vue";
import "highlight.js/styles/github-dark.css";
import "katex/dist/katex.min.css";

const escapeHtml = (s: string) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const mdParser = new Marked(
  markedHighlight({
    langPrefix: "hljs language-",
    // marked inserts the return value verbatim. Returning an empty string
    // in the untagged path would silently drop the code, so escape it.
    highlight(code, lang) {
      if (lang && hljs.getLanguage(lang)) {
        return hljs.highlight(code, { language: lang }).value;
      }
      return escapeHtml(code);
    },
  }),
);
mdParser.setOptions({ gfm: true, breaks: true, async: false });
mdParser.use(
  markedKatex({
    // Stream chunks contain unclosed $...$ until the delimiter arrives.
    // KaTeX must render bad TeX in error color instead of throwing.
    throwOnError: false,
    output: "html",
  }),
);
// Model output is untrusted: sanitize before v-html. file:// stays
// allowed because onContentClick intercepts clicks and routes them
// through the validated open_path command, not the webview.
const ALLOWED_URI =
  /^(?:(?:(?:f|ht)tps?|mailto|tel|callto|sms|cid|xmpp|file):|[^a-z]|[a-z+.\-]+(?:[^a-z+.\-:]|$))/i;
function renderMd(text: string): string {
  return DOMPurify.sanitize(mdParser.parse(text), { ALLOWED_URI_REGEXP: ALLOWED_URI });
}

// Every link click leaves the webview: http(s) opens the system browser,
// file:// opens Explorer or the default app. The chat window never
// navigates itself.
async function onContentClick(e: MouseEvent) {
  const a = (e.target as HTMLElement).closest("a");
  if (!a) return;
  e.preventDefault();
  const href = a.getAttribute("href") || "";
  try {
    await invoke("open_path", { target: href });
  } catch (err) {
    addMsg(`Could not open ${href}: ${err}`, "dim");
  }
}

const win = getCurrentWindow();

// Backend-produced facts about one tool call. The backend also ships a
// model-facing transcript; glyphs and wording in this file are UI-only.
interface ToolReport {
  tool: string;
  subject: string;
  detail?: string;
  shell?: string;
  cwd?: string;
  background?: boolean;
  from_tag?: string;
  ok: boolean;
  error?: string;
  status?: string;
  pid?: number;
  wall_ms: number;
  tag?: string;
  count?: number;
  unit?: string;
  body?: { kind: string; text: string };
}

// One redaction pass summary. The payload carries class labels and
// counts, never a redacted value.
interface RedactReport {
  count: number;
  classes: [string, number][];
}

interface Msg {
  id: number;
  text: string;
  cls: string;
  // Pending tool lines are replaced in place when the result arrives.
  toolName?: string;
  done?: boolean;
  tool?: ToolReport;
  // View-only: a reopened conversation shows the divider without a count.
  folded?: number;
  // View-only: a reopened conversation has no chip.
  redact?: RedactReport;
}

const TOOL_GLYPH: Record<string, string> = {
  run_command: "$",
  get_command_output: "◷",
  write_file: "✎",
  edit_file: "✎",
  read_file: "→",
  list_dir: "▸",
  search_files: "⌕",
  list_processes: "≣",
  kill_command: "✖",
  load_skill: "📖",
  read_skill_resource: "📖",
};

function baseName(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i >= 0 ? p.slice(i + 1) : p;
}

function reportIdentity(r: ToolReport): string {
  const glyph = TOOL_GLYPH[r.tool] ?? "🔧";
  switch (r.tool) {
    case "run_command": {
      let s = `$ ${r.subject}`;
      if (r.shell) s += ` [${r.shell}]`;
      if (r.cwd) s += ` in ${baseName(r.cwd)}`;
      if (r.background) s += " [bg]";
      return s;
    }
    case "get_command_output":
      return `◷ output of pid ${r.subject}`;
    case "read_file":
      return `→ ${baseName(r.subject)}${r.detail ? ":" + r.detail : ""}`;
    case "edit_file":
      return `✎ ${baseName(r.subject)}${r.from_tag ? " #" + r.from_tag : ""}${r.detail ? " " + r.detail : ""}`;
    case "write_file":
      return `✎ ${baseName(r.subject)}${r.count != null ? ` (${r.count} ${r.unit})` : ""}`;
    case "list_dir":
      return `▸ ${r.subject}`;
    case "search_files":
      return `⌕ "${r.subject}"${r.detail ? " under " + r.detail : ""}`;
    case "list_processes":
      return "≣ running processes";
    case "kill_command":
      return `✖ kill pid ${r.subject}`;
    case "load_skill":
      return `📖 load_skill ${r.subject}`;
    case "read_skill_resource":
      return `📖 ${r.subject}`;
    default:
      return `${glyph} ${r.tool}${r.detail ? " " + r.detail : ""}`;
  }
}

function reportFooter(r: ToolReport): string {
  const parts: string[] = [];
  if (r.status) parts.push(r.status);
  if (r.wall_ms > 0) parts.push((r.wall_ms / 1000).toFixed(1) + "s");
  if (r.tag) parts.push("#" + r.tag);
  // write_file's header already states the byte count; don't repeat it.
  if (r.count != null && r.unit && r.tool !== "write_file") parts.push(`${r.count} ${r.unit}`);
  if (r.pid != null && r.status) parts.push("pid " + r.pid);
  const mark = r.ok ? "✅" : `❌ ${r.error ?? "failed"}`;
  return parts.length ? `${mark} ${parts.join(" · ")}` : mark;
}

const SUBJECT_ARG: Record<string, string> = {
  run_command: "command",
  read_file: "path",
  write_file: "path",
  edit_file: "path",
  list_dir: "path",
  search_files: "pattern",
  kill_command: "pid",
  get_command_output: "pid",
  load_skill: "name",
  read_skill_resource: "path",
};

function pendingReport(name: string, args: Record<string, unknown>): ToolReport {
  const str = (k: string) => (args[k] == null ? "" : String(args[k]));
  const r: ToolReport = { tool: name, subject: str(SUBJECT_ARG[name] ?? ""), ok: true, wall_ms: 0 };
  if (name === "run_command") {
    r.subject = r.subject.replace(/\s+/g, " ").trim().slice(0, 160);
    r.shell = str("shell") || undefined;
    r.cwd = str("cwd") || undefined;
    r.background = args["background"] === true ? true : undefined;
  } else if (name === "read_file" && args["offset"] != null) {
    const off = Number(args["offset"]);
    const end = args["limit"] != null ? off + Number(args["limit"]) - 1 : "…";
    r.detail = `${off}-${end}`;
  } else if (name === "edit_file") {
    r.from_tag = str("tag") || undefined;
    const heads = str("ops")
      .split("\n")
      .filter((l) => l && !l.startsWith("+"))
      .join(", ");
    r.detail = heads.slice(0, 90) || undefined;
  } else if (name === "write_file") {
    r.count = new TextEncoder().encode(str("content")).length;
    r.unit = "bytes";
  } else if (name === "search_files") {
    r.detail = str("root") || undefined;
  }
  return r;
}

const messages = ref<Msg[]>([]);
const input = ref("");
let nextId = 0;
const scrollerRef = ref();
const isThinking = ref(false);
const inputRef = ref<HTMLElement>();
let liveMsgId = -1;
let pendingRedact: RedactReport | undefined;
const spinnerChar = ref("⠋");
// The Privacy Model flags a message; you decide. The card takes the composer's
// place, because the send waits on your answer.
interface AuditWindow {
  start: number;
  end: number;
  score: number;
}
interface AuditCard {
  message: string;
  score: number;
  windows: AuditWindow[];
}
const audit = ref<AuditCard | null>(null);
const redactions = ref<string[]>([]);
const selection = ref("");

// Rust names the cause, and the console writes the sentence, because these words
// are the product's. A hold is your decision, or an outage you fix in Settings, so
// a hold is not a fault inside First Mate.
const HELD_TEXT: Record<string, (detail: string) => string> = {
  denied: () =>
    "You denied that message, so First Mate did not send it. Nothing reached the Agent Model.",
  stopped: () => "You stopped the run before you answered, so First Mate did not send the message.",
  outage: (host) =>
    `First Mate sent nothing, because the Privacy Model at ${host} did not answer. ` +
    "Start that server, or turn off the Privacy Model in Settings.",
};

// Ranges count code points, so Array.from splits the message the way the
// backend counted it. A window over the whole message marks nothing, because it
// says nothing about where to read.
function auditParts(card: AuditCard) {
  const chars = Array.from(card.message);
  const parts: { text: string; mark: boolean }[] = [];
  let at = 0;
  for (const w of [...card.windows].sort((a, b) => a.start - b.start)) {
    const start = Math.max(w.start, at);
    const end = Math.min(w.end, chars.length);
    if (start >= end || (w.start === 0 && w.end >= chars.length)) continue;
    if (start > at) parts.push({ text: chars.slice(at, start).join(""), mark: false });
    parts.push({ text: chars.slice(start, end).join(""), mark: true });
    at = end;
  }
  if (at < chars.length) parts.push({ text: chars.slice(at).join(""), mark: false });
  return parts;
}

function readSelection() {
  selection.value = window.getSelection()?.toString() ?? "";
}

// A selection shorter than four characters would replace ordinary words in the
// outbound copy, so the backend rejects it too.
function redactSelection() {
  const value = selection.value.trim();
  if (value.length < 4) return;
  redactions.value = [...redactions.value, value];
  selection.value = "";
}

async function answerAudit(approved: boolean) {
  if (!audit.value) return;
  audit.value = null;
  const kept = approved ? redactions.value : [];
  redactions.value = [];
  await invoke("answer_audit", { approved, redactions: kept });
}

interface ConvInfo {
  name: string;
  path: string;
  modified: number;
}
interface ConvMsg {
  role: string;
  text: string;
  report?: ToolReport;
}
const panelOpen = ref(false);
const wizardOpen = ref(false);
const conversations = ref<ConvInfo[]>([]);
const convName = ref<string | null>(null);
// Temporary conversations: never written to disk, vanish on New/restart.
const tempChat = ref(false);
// Rust owns the Elevated Control state. The agent moves the state with
// elevate and delevate too, reported via elevation_changed events.
const elevActive = ref(false);
const elevCount = ref(0);

function focusInput() {
  const el = inputRef.value?.querySelector("textarea") || inputRef.value?.querySelector("input");
  if (el) el.focus();
}

interface LlmSettings {
  provider: string;
  model: string;
  api_key: string;
  base_url: string;
  context_window?: number;
  compact_at_percent?: number;
}
let llmSettings: LlmSettings | null = null;

function addMsg(text: string, cls = "", extra: Partial<Msg> = {}) {
  messages.value.push({ id: nextId++, text, cls, ...extra });
}

// Divider expansion is view state, not transcript data. Content in
// `messages` fires the deep scroll watcher. A divider click would then
// jump the view to the bottom.
const openSummaries = ref(new Set<number>());

function toggleSummary(id: number) {
  if (openSummaries.value.has(id)) openSummaries.value.delete(id);
  else openSummaries.value.add(id);
}

// The divider marks when summarizing happened, not a content boundary.
// Rows above the divider stay visible.
function foldTitle(m: Msg): string {
  const folded = m.folded ? ` ${m.folded} earlier messages were folded into it.` : "";
  return openSummaries.value.has(m.id)
    ? `Hide the summary.${folded}`
    : `Show the summary First Mate works from instead of those messages.${folded}`;
}

// `dim` notes and errors are UI-only: they never persist and never reach
// the model. A `summary` divider persists so a reopened conversation
// still shows the folded text.
function isTranscriptRow(m: Msg): boolean {
  return ["user", "assistant", "tool", "summary"].includes(m.cls) && !!m.text.trim();
}

function persistConversation() {
  if (tempChat.value) return;
  // Tool reports persist so a resumed conversation sees work already done.
  const turns = messages.value
    .filter(isTranscriptRow)
    .map((m) => ({ role: m.cls, text: m.text, report: m.done ? m.tool : undefined }));
  if (turns.length === 0) return;
  if (!convName.value) {
    const first = turns.find((t) => t.role === "user");
    convName.value = (first ? first.text : "Chat").replace(/\s+/g, " ").trim().slice(0, 48);
  }
  invoke<string>("save_conversation", { name: convName.value, messages: turns })
    .then(() => refreshConversations())
    .catch((e) => console.error("save failed", e));
}

function refreshConversations() {
  return invoke<ConvInfo[]>("list_conversations")
    .then((list) => {
      conversations.value = list;
    })
    .catch((e) => console.error("list failed", e));
}

function openConversation(c: ConvInfo) {
  invoke<ConvMsg[]>("load_conversation", { path: c.path })
    .then((msgs) => {
      // Saved roles equal `cls` values 1:1. A persisted `summary` row
      // therefore comes back as a divider, not a chat bubble.
      messages.value = msgs.map((m) => ({
        id: nextId++,
        text: m.text,
        cls: m.role,
        ...(m.role === "tool" && m.report ? { tool: m.report, done: true } : {}),
      }));
      convName.value = c.name;
      tempChat.value = false;
      panelOpen.value = false;
      refreshBehavior();
    })
    .catch((e) => console.error("load failed", e));
}

function newChat() {
  messages.value = [];
  convName.value = null;
  tempChat.value = false;
  panelOpen.value = false;
  refreshBehavior();
}

// Prompt and skill ads are content files. The window is hidden on close,
// not destroyed, so a mount-once fetch would serve a stale prompt
// indefinitely.
function refreshBehavior() {
  invoke<string>("get_system_prompt").then((p) => {
    systemPromptBase.value = p;
  });
  invoke<SkillInfo[]>("list_skills").then((skills) => {
    if (skills.length === 0) return;
    const list = skills.map((s) => `- ${s.name}: ${s.description}`).join("\n");
    skillsAds.value =
      `\n\n## Skills (loaded on demand)\nAvailable skills:\n${list}\n` +
      `When a task matches a skill's domain, call load_skill("<name>") to get its ` +
      `full instructions before acting; read reference files with ` +
      `read_skill_resource("<skill>/<relative/path>"). Tools documented by a ` +
      `skill refuse calls until that skill is loaded.`;
  });
}

function newTempChat() {
  messages.value = [];
  convName.value = null;
  tempChat.value = true;
  panelOpen.value = false;
  refreshBehavior();
  addMsg("Temporary conversation — nothing here is saved to disk.", "dim");
}

function deleteConversation(c: ConvInfo) {
  invoke("delete_conversation", { path: c.path })
    .then(() => {
      if (c.name === convName.value) newChat();
      return refreshConversations();
    })
    .catch((e) => console.error("delete failed", e));
}

// The console caches the provider, so a wizard save reaches the next message
// only through this call.
async function loadSettings(): Promise<LlmSettings> {
  const s = await invoke<{ llm: LlmSettings }>("get_settings");
  llmSettings = s.llm;
  return s.llm;
}

function finishSetup() {
  wizardOpen.value = false;
  loadSettings();
}

onMounted(() => {
  refreshConversations().then(() => {
    if (conversations.value.length > 0) {
      openConversation(conversations.value[0]);
    }
  });
  invoke<string>("get_hotkey").then((hotkey) => {
    addMsg(`First Mate is online. Summon with ${hotkey}.`, "dim");
  });
  loadSettings().then((llm) => {
    if (!llm.model) wizardOpen.value = true;
  });
  invoke<boolean>("recovery_key_status").then((pending) => {
    if (pending) wizardOpen.value = true;
  });
  listen("open_setup", () => (wizardOpen.value = true));
  invoke<{ active: boolean; commands: number }>("elevation_status").then((s) => {
    elevActive.value = s.active;
    elevCount.value = s.commands;
  });
  listen<{ active: boolean; pid?: number; commands?: number }>("elevation_changed", (e) => {
    elevActive.value = e.payload.active;
    elevCount.value = e.payload.commands ?? 0;
    addMsg(
      e.payload.active
        ? `Elevated Control ON — admin session (pid ${e.payload.pid}). You can end it any time with the ELEV button.`
        : "Elevated Control OFF — back to non-elevated only.",
      "dim",
    );
  });
  refreshBehavior();
  nextTick(() => focusInput());
  listen("tauri://focus", () => focusInput());
  listen<{ name: string; args: Record<string, unknown> }>("tool_call", (event) => {
    const { name, args } = event.payload;
    const tool = pendingReport(name, args);
    addMsg(reportIdentity(tool), "tool", {
      toolName: name,
      tool: { ...tool, status: undefined, tag: undefined },
    });
  });
  listen<{ name: string; transcript: string; report?: ToolReport }>("tool_result", (event) => {
    const { name, transcript, report } = event.payload;
    let matched = false;
    for (let i = messages.value.length - 1; i >= 0; i--) {
      const m = messages.value[i];
      if (m.cls === "tool" && m.toolName === name && !m.done) {
        messages.value[i] = { ...m, text: transcript, tool: report, done: true };
        matched = true;
        break;
      }
    }
    // An unmatched result is an async announcement, e.g. a background job
    // that finished after its turn. It becomes its own row so the model
    // resumes with the announcement in context.
    if (!matched) addMsg(transcript, "tool", { toolName: name, done: true, tool: report });
  });
  listen<{ text: string }>("stream_chunk", (event) => {
    if (liveMsgId < 0) {
      addMsg("", "assistant");
      liveMsgId = nextId - 1;
    }
    const idx = messages.value.findIndex((m) => m.id === liveMsgId);
    if (idx >= 0) {
      messages.value[idx] = {
        id: liveMsgId,
        text: messages.value[idx].text + event.payload.text,
        cls: "assistant",
      };
    }
  });
  // A tool turn ends the live message. The next turn streams into a new
  // message.
  listen("stream_reset", () => {
    liveMsgId = -1;
  });
  // Mid-run compaction (docs/COMPACTION.md): the backend folded the
  // oldest rows into a brief so the request fits the context window. The next
  // request resumes after the saved `summary` row.
  listen<{ summary: string; tokensBefore: number; dropped: number }>("compact", (event) => {
    addMsg(event.payload.summary, "summary", { folded: event.payload.dropped });
  });
  // The redaction pass reports once per run, before the run's final
  // commit. Chunks still arrive after it and rebuild the live row.
  // submit() attaches the payload when it commits the final message.
  listen<RedactReport>("redact", (event) => {
    pendingRedact = event.payload;
  });
  listen<{ message: string; score: number; windows: AuditWindow[] }>("audit", (event) => {
    audit.value = {
      message: event.payload.message,
      score: event.payload.score,
      windows: event.payload.windows ?? [],
    };
    redactions.value = [];
    selection.value = "";
  });
});

const systemPromptBase = ref("");

interface SkillInfo {
  name: string;
  description: string;
}
const skillsAds = ref("");
const BRAILLE = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
let spinnerInterval: number | null = null;

watch(isThinking, (thinking) => {
  if (thinking) {
    let i = 0;
    spinnerChar.value = BRAILLE[0];
    spinnerInterval = window.setInterval(() => {
      spinnerChar.value = BRAILLE[i % BRAILLE.length];
      i++;
    }, 100);
  } else if (spinnerInterval !== null) {
    clearInterval(spinnerInterval);
    spinnerInterval = null;
  }
});

function onInputKey(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void submit();
  }
}

// Capability is earned once per CONVERSATION, not per request. The tool
// ledger records loaded skills, so gated tools stay unlocked across turns
// and resumes. The backend re-checks the set at dispatch.
function loadedSkills(): string[] {
  const names = new Set<string>();
  for (const m of messages.value) {
    const r = m.tool;
    if (m.cls === "tool" && m.done && r?.tool === "load_skill" && r.ok && r.subject) {
      names.add(r.subject);
    }
  }
  return [...names];
}

async function submit() {
  const text = input.value.trim();
  if (!text) return;
  // One agent run at a time. Enter while running must not start a second
  // loop.
  if (isThinking.value) return;
  addMsg(text, "user");
  // The row just added travels in `message`, so it must not repeat in
  // `priorHistory`.
  const currentId = nextId - 1;
  input.value = "";

  if (!llmSettings || !llmSettings.provider || !llmSettings.model) {
    addMsg("(no provider configured — open Settings to configure an LLM)", "dim");
    return;
  }

  // Send the whole transcript, minus the message just typed. No row cap
  // here. The backend windows by tokens and cuts at turn boundaries
  // (docs/COMPACTION.md). A row count here would drop the user's task.
  // Tool rows travel as assistant text so a resumed run sees work
  // already done. A `summary` row travels as the summary that replaced
  // the rows above it.
  const priorHistory = messages.value
    .filter((m) => m.id !== currentId && isTranscriptRow(m))
    .map((m) => ({ role: m.cls === "tool" ? "assistant" : m.cls, content: m.text }));

  isThinking.value = true;
  liveMsgId = -1;
  pendingRedact = undefined;

  try {
    const final = await invoke<string>("chat_with_llm", {
      provider: llmSettings.provider,
      baseUrl: llmSettings.base_url,
      apiKey: llmSettings.api_key,
      model: llmSettings.model,
      message: text,
      priorHistory,
      systemPrompt: systemPromptBase.value + skillsAds.value,
      loadedSkills: loadedSkills(),
      contextWindow: llmSettings.context_window ?? 0,
      // The percent caps how full the window may get before summarizing.
      compactAtPercent: llmSettings.compact_at_percent ?? 85,
    });
    if (liveMsgId < 0) {
      if (final.trim()) addMsg(final, "assistant", { redact: pendingRedact });
      else addMsg("(no response)", "dim");
    } else if (pendingRedact) {
      const idx = messages.value.findIndex((m) => m.id === liveMsgId);
      if (idx >= 0) messages.value[idx] = { ...messages.value[idx], redact: pendingRedact };
    }
  } catch (e) {
    const msg = typeof e === "string" ? e : e && e.message ? e.message : String(e);
    const held = /^held:(\w+)(?: (.*))?$/.exec(msg);
    const sentence = held ? HELD_TEXT[held[1]]?.(held[2] ?? "") : undefined;
    if (held?.[1] === "outage" && sentence) {
      addMsg(`Error: ${sentence}`, "error");
    } else if (sentence) {
      addMsg(sentence, "dim");
    } else {
      addMsg(`Error: ${msg}\n\nThe agent stopped.`, "error");
    }
  } finally {
    isThinking.value = false;
    liveMsgId = -1;
    pendingRedact = undefined;
    audit.value = null;
    persistConversation();
  }
}

// The agent loop stops at the next turn/tool boundary and returns
// "(stopped)".
function stopRun() {
  invoke("stop_chat").catch((e) => console.error("stop failed", e));
}

function hide() {
  console.log("hide() called");
  win
    .hide()
    .then(() => console.log("hidden"))
    .catch((e) => console.error("hide error", e));
}

function openSettings() {
  invoke("open_settings").catch((e) => console.error("open settings failed", e));
}

async function toggleElevation() {
  try {
    if (elevActive.value) addMsg(await invoke<string>("delevate"), "dim");
    else addMsg(await invoke<string>("elevate"), "dim");
  } catch (e) {
    addMsg(String(e), "error");
  }
}

// DynamicScroller measures item sizes asynchronously. After a bulk load,
// scrollHeight is a small estimate that settles at the estimated bottom
// before real sizes land. Pin every frame until scrollHeight is stable
// and the view is at the bottom. Reaching the bottom once is not proof
// of arrival.
let userScrollAbort = false;
function scrollToBottom() {
  nextTick(() => {
    const el = scrollerRef.value?.$el as HTMLElement | undefined;
    if (!el) return;
    scrollerRef.value?.scrollToItem?.(messages.value.length - 1);
    userScrollAbort = false;
    let tries = 0;
    let stable = 0;
    let lastSh = -1;
    const pin = () => {
      el.scrollTop = el.scrollHeight;
      stable = el.scrollHeight === lastSh ? stable + 1 : 0;
      lastSh = el.scrollHeight;
      const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight <= 2;
      if (!userScrollAbort && !(stable > 20 && atBottom) && ++tries < 600) {
        requestAnimationFrame(pin);
      }
    };
    pin();
  });
}
watch(messages, scrollToBottom, { deep: true });
// A wheel event means the user controls the scroll position. Stop pinning
// instead of pulling the view back to the bottom.
window.addEventListener("wheel", () => (userScrollAbort = true));

window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (wizardOpen.value) return; // dismissed only via Skip/Finish
    if (panelOpen.value) panelOpen.value = false;
    else hide();
  } else if (e.ctrlKey && !e.altKey && e.key.toLowerCase() === "n") {
    e.preventDefault();
    if (e.shiftKey) newTempChat();
    else newChat();
  }
});
</script>

<template>
  <n-config-provider :theme="darkTheme">
    <div class="chat">
      <header class="titlebar">
        <span class="title">⚓️ First Mate<span v-if="tempChat" class="temp-badge">TEMP</span></span>
        <button
          class="panel-toggle"
          :title="panelOpen ? 'Hide chats' : 'Show chats'"
          @click="
            panelOpen = !panelOpen;
            refreshConversations();
          "
        >
          &#9776;
        </button>
        <button
          class="elev-toggle"
          :class="{ on: elevActive }"
          :title="
            elevActive
              ? `Elevated Control ON — ${elevCount} admin command(s) run. Click to give it up.`
              : 'Elevated Control OFF — the agent has no admin rights. ON requires your UAC consent.'
          "
          @click="toggleElevation"
        >
          &#9650;{{ elevActive ? ` ${elevCount}` : "" }}
        </button>
        <button class="settings-gear" title="Settings" @click="openSettings">&#9881;</button>
        <button class="close" @click="hide">&#10005;</button>
      </header>
      <div class="body">
        <div class="main">
          <DynamicScroller ref="scrollerRef" :items="messages" key-field="id" class="scroller">
            <template #default="{ item, active }">
              <DynamicScrollerItem
                :item="item"
                :active="active"
                :size-dependencies="[item.text, openSummaries.has(item.id)]"
              >
                <!-- v-html is safe: renderMd() sanitizes with DOMPurify. -->
                <!-- eslint-disable vue/no-v-html -->
                <div
                  v-if="item.cls === 'assistant'"
                  class="msg md assistant"
                  @click="onContentClick"
                  v-html="renderMd(item.text)"
                ></div>
                <!-- eslint-enable vue/no-v-html -->
                <div v-else-if="item.cls === 'tool' && item.tool" class="rb-wrap">
                  <div class="msg tool report" :class="{ failed: item.done && !item.tool.ok }">
                    <div class="rb-head">{{ reportIdentity(item.tool) }}</div>
                    <pre v-if="item.tool.body" class="rb-body"><span
                      v-for="(ln, i) in item.tool.body.text.split('\n')"
                      :key="i"
                      class="rb-line"
                      :class="item.tool.body.kind === 'diff' ? (ln.startsWith('+') ? 'add' : ln.startsWith('-') ? 'del' : '') : ''"
                    >{{ ln }}
</span></pre>
                    <div class="rb-foot" :class="{ pending: !item.done }">
                      {{ item.done ? reportFooter(item.tool) : "running…" }}
                    </div>
                  </div>
                </div>
                <div
                  v-else-if="item.cls === 'summary'"
                  class="msg summary"
                  :class="{ open: openSummaries.has(item.id) }"
                >
                  <button
                    class="summary-rule"
                    :title="foldTitle(item)"
                    @click="toggleSummary(item.id)"
                  >
                    ── First Mate summarized the earlier chat so it could keep going ──
                  </button>
                  <pre v-if="openSummaries.has(item.id)" class="summary-body">{{ item.text }}</pre>
                </div>
                <div v-else class="msg" :class="item.cls">{{ item.text }}</div>
                <div v-if="item.redact" class="redact-row">
                  <n-popover trigger="click" placement="bottom-start">
                    <template #trigger>
                      <button class="redact-chip">Redacted {{ item.redact.count }} items</button>
                    </template>
                    <div
                      v-for="[label, n] in item.redact.classes"
                      :key="label"
                      class="redact-class"
                    >
                      {{ label }} — {{ n }}
                    </div>
                  </n-popover>
                </div>
              </DynamicScrollerItem>
            </template>
          </DynamicScroller>
          <div
            v-if="isThinking && !audit"
            class="msg dim thinking stoppable"
            title="Click to stop the agent"
            @click="stopRun"
          >
            First Mate {{ spinnerChar }} — click to stop
          </div>
          <div v-if="audit" class="audit" @mouseup="readSelection" @keyup="readSelection">
            <div class="audit-head">
              The Privacy Model scored this message {{ audit.score.toFixed(2) }}. Select the
              sensitive text and redact it, or deny the send.
            </div>
            <div class="audit-body">
              <template v-for="(part, i) in auditParts(audit)" :key="i">
                <mark v-if="part.mark">{{ part.text }}</mark>
                <template v-else>{{ part.text }}</template>
              </template>
            </div>
            <ul v-if="redactions.length" class="audit-marks">
              <li v-for="(value, i) in redactions" :key="i">{{ value }}</li>
            </ul>
            <div class="audit-foot">
              <button
                class="audit-btn"
                :disabled="selection.trim().length < 4"
                title="Select at least four characters to redact."
                @click="redactSelection"
              >
                Redact selection
              </button>
              <button class="audit-btn" @click="answerAudit(false)">Deny</button>
              <button class="audit-btn approve" @click="answerAudit(true)">Approve</button>
            </div>
          </div>
          <div v-if="!audit" ref="inputRef" class="input-row">
            <n-input
              v-model:value="input"
              type="textarea"
              autosize
              :placeholder="
                isThinking
                  ? 'First Mate is working… type to queue'
                  : 'Type a message… (Shift+Enter for newline)'
              "
              @keydown="onInputKey"
            />
          </div>
        </div>
        <aside v-if="panelOpen" class="side">
          <div class="side-head">
            <span>Chats</span>
            <span class="head-actions">
              <button
                class="new-chat temp"
                title="Start a temporary conversation — never saved (Ctrl+Shift+N)"
                @click="newTempChat"
              >
                <span class="plus">+</span> Temp
              </button>
              <button class="new-chat" title="Start a new conversation (Ctrl+N)" @click="newChat">
                <span class="plus">+</span> New
              </button>
            </span>
          </div>
          <ul class="conv-list">
            <li
              v-for="c in conversations"
              :key="c.path"
              :class="{ active: c.name === convName }"
              :title="c.name"
              @click="openConversation(c)"
            >
              <span class="conv-name">{{ c.name }}</span>
              <button
                class="conv-delete"
                title="Delete conversation"
                @click.stop="deleteConversation(c)"
              >
                &times;
              </button>
            </li>
          </ul>
        </aside>
      </div>
      <SetupWizard v-if="wizardOpen" @done="finishSetup" />
    </div>
  </n-config-provider>
</template>

<style scoped>
/* A status marker, not a call to action: dim gray, quiet border. */
.redact-row {
  padding: 0 12px 12px;
}
.redact-chip {
  padding: 1px 8px;
  border: 1px solid #2c2c31;
  border-radius: 4px;
  background: none;
  color: #8a8f98;
  font: inherit;
  font-size: 12px;
  cursor: pointer;
}
.redact-chip:hover {
  color: #c9ced6;
}
.redact-class {
  font-family: monospace;
  font-size: 12px;
  line-height: 1.5;
}

/* The send waits on this card, so the card takes the composer's place. */
.audit {
  margin: 8px 12px;
  padding: 10px 12px;
  border: 1px solid #3a3a41;
  border-left: 3px solid #f0a23a;
  border-radius: 6px;
  background: #1f1f23;
}
.audit-head {
  margin-bottom: 8px;
  color: #f0a23a;
  font-size: 12px;
}
.audit-body {
  max-height: 30vh;
  overflow-y: auto;
  white-space: pre-wrap;
  font-size: 13px;
  user-select: text;
}
.audit-body mark {
  background: #4a3a12;
  color: #f5d78a;
}
.audit-marks {
  margin: 8px 0 0;
  padding-left: 18px;
  color: #8a8f98;
  font-family: monospace;
  font-size: 12px;
}
.audit-foot {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  margin-top: 10px;
}
.audit-btn {
  padding: 3px 12px;
  border: 1px solid #3a3a41;
  border-radius: 4px;
  background: none;
  color: #c9ced6;
  font: inherit;
  font-size: 12px;
  cursor: pointer;
}
.audit-btn:disabled {
  color: #5a5f66;
  cursor: default;
}
.audit-btn.approve {
  border-color: #2f6b3f;
  color: #7ac38a;
}
</style>
