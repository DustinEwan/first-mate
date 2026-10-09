<script setup lang="ts">
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ref, onMounted, watch, nextTick } from "vue";
import { NConfigProvider, NInput, darkTheme } from "naive-ui";
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
    // Tagged fences only; the return value is inserted verbatim, so the
    // untagged/unknown path must escape the code itself (empty string would
    // silently drop it).
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
    // Streaming: an unclosed $...$ mid-chunk stays literal text until the
    // delimiter closes; bad TeX renders in error color instead of throwing.
    throwOnError: false,
    output: "html",
  }),
);
// Model output is untrusted: sanitize before it reaches v-html. file:// is
// allowed through on purpose — clicks are intercepted (onContentClick) and
// opened via the validated open_path command, never by the webview itself.
const ALLOWED_URI =
  /^(?:(?:(?:f|ht)tps?|mailto|tel|callto|sms|cid|xmpp|file):|[^a-z]|[a-z+.\-]+(?:[^a-z+.\-:]|$))/i;
function renderMd(text: string): string {
  return DOMPurify.sanitize(mdParser.parse(text), { ALLOWED_URI_REGEXP: ALLOWED_URI });
}

// All link clicks leave the webview: http(s) -> system browser, file:// ->
// Explorer/default app. Navigation inside the chat window is never allowed.
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

// Structured facts about a tool call, produced by the backend. All glyphs,
// layout, and wording below are UI presentation; the backend only ships
// facts (plus a separate model-facing transcript it owns).
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

interface Msg {
  id: number;
  text: string;
  cls: string;
  // Tool ledger bookkeeping: which tool the line belongs to, and whether
  // its result has landed (pending lines get replaced by the result block).
  toolName?: string;
  done?: boolean;
  // Structured facts: rendered as sections of one bordered panel.
  tool?: ToolReport;
  // Compaction bookkeeping: how many rows this divider folded into the brief.
  // View-only — a reopened conversation shows the divider without a count.
  folded?: number;
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

// Provisional facts from call args, shown while the tool runs. The finished
// report replaces it wholesale.
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
const spinnerChar = ref("⠋");

// Conversation persistence + side panel.
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
// First-run stepper: shown when no model is configured; tray can reopen it.
const wizardOpen = ref(false);
const conversations = ref<ConvInfo[]>([]);
const convName = ref<string | null>(null);
// Temporary conversations: never written to disk, vanish on New/restart.
const tempChat = ref(false);
// Elevated Control: the admin session's state, mirrored from Rust. The
// toggle is the user's hand on it; the agent's elevate/delevate move it
// too, via the elevation_changed event.
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
  // Compaction (docs/COMPACTION.md): 0 = read the window off the model
  // endpoint. Settings files written before compaction carry neither.
  context_window?: number;
  compact_at_percent?: number;
}
let llmSettings: LlmSettings | null = null;

function addMsg(text: string, cls = "", extra: Partial<Msg> = {}) {
  messages.value.push({ id: nextId++, text, cls, ...extra });
}

// Divider expansion is view state, not transcript data: keeping it out of
// `messages` means clicking a divider doesn't trip the deep
// scroll-to-bottom watcher and drag the user back to the live edge.
const openSummaries = ref(new Set<number>());

function toggleSummary(id: number) {
  if (openSummaries.value.has(id)) openSummaries.value.delete(id);
  else openSummaries.value.add(id);
}

// The divider is written for the person reading the chat, not for the ledger:
// plain words about what happened, with the mechanics (how many messages went
// into the summary) left to the tooltip. It marks when the summarizing
// happened, not a boundary — everything above it stays visible.
function foldTitle(m: Msg): string {
  const folded = m.folded ? ` ${m.folded} earlier messages were folded into it.` : "";
  return openSummaries.value.has(m.id)
    ? `Hide the summary.${folded}`
    : `Show the summary First Mate works from instead of those messages.${folded}`;
}

// What counts as the transcript: the turns the model saw (or produced).
// `dim` notes and errors are UI-only, so they neither persist nor go back
// on the wire. `summary` is a compaction divider: it persists as its own
// row so a reopened conversation still shows what was summarized away.
function isTranscriptRow(m: Msg): boolean {
  return ["user", "assistant", "tool", "summary"].includes(m.cls) && !!m.text.trim();
}

function persistConversation() {
  if (tempChat.value) return; // temp chats live only in this window's memory
  // The tool ledger persists too: a resumed conversation can see what was
  // already inspected, verified, and executed instead of re-doing it.
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
      // role ↔ cls is 1:1, so a persisted "summary" row comes back as the
      // compaction divider — not a chat bubble, and not dropped.
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

// Prompt and skill ads are CONTENT: re-read them at every conversation
// start so editing prompts/AGENTS.md or prompts/skills/ takes effect on the next chat —
// closing the window only hides it, so a mount-once fetch would serve a
// stale prompt for the life of the process.
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

onMounted(() => {
  // Restore the most recent conversation so chats survive restarts.
  refreshConversations().then(() => {
    if (conversations.value.length > 0) {
      openConversation(conversations.value[0]);
    }
  });
  invoke<string>("get_hotkey").then((hotkey) => {
    addMsg(`First Mate is online. Summon with ${hotkey}.`, "dim");
  });
  invoke<{ llm: LlmSettings }>("get_settings").then((s) => {
    llmSettings = s.llm;
    if (!s.llm.model) wizardOpen.value = true;
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
  // Focus the input when the window is shown/focused.
  nextTick(() => focusInput());
  listen("tauri://focus", () => focusInput());
  // Show tool calls in real-time as the agent makes them: the pending panel
  // is built from the call args (UI-side provisional facts).
  listen<{ name: string; args: Record<string, unknown> }>("tool_call", (event) => {
    const { name, args } = event.payload;
    const tool = pendingReport(name, args);
    addMsg(reportIdentity(tool), "tool", {
      toolName: name,
      tool: { ...tool, status: undefined, tag: undefined },
    });
  });
  // Replace the pending panel with the finished report. The transcript (the
  // backend's model-facing flat form) becomes the persisted text; the UI
  // renders only the structured facts.
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
    // Unmatched results are async announcements (e.g. a background job that
    // finished after its turn): they get their own report so the model
    // resumes with them as context.
    if (!matched) addMsg(transcript, "tool", { toolName: name, done: true, tool: report });
  });
  // Stream the assistant text in as it arrives, creating the live message on
  // the first chunk.
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
  // A tool-call turn: finalize the current live message so the next turn
  // streams into a fresh one.
  listen("stream_reset", () => {
    liveMsgId = -1;
  });
  // Mid-run compaction (docs/COMPACTION.md): the backend folded the oldest
  // rows of this run into a brief so the request stays inside the window. The
  // divider lands after whatever tool rows have already streamed, so it marks
  // when the fold happened rather than where the transcript begins: the rows
  // above it stay in the view, and the newest tail stays on the wire under
  // them. The saved `summary` row is what the next request resumes after.
  listen<{ summary: string; tokensBefore: number; dropped: number }>("compact", (event) => {
    addMsg(event.payload.summary, "summary", { folded: event.payload.dropped });
  });
});

// Behavior text comes from AGENTS.md (repo prompts/ or ~/.firstmate), loaded via
// get_system_prompt; the Rust fallback applies only when no file exists.
const systemPromptBase = ref("");

interface SkillInfo {
  name: string;
  description: string;
}
// Stage 1 of progressive disclosure (docs/SKILLS.md): advertise skills by
// name + description; the agent loads bodies on demand via load_skill.
const skillsAds = ref("");
// Braille spinner for the standalone thinking indicator (a div, not a message).
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

// Enter submits; Shift+Enter inserts a newline (the textarea grows).
function onInputKey(e: KeyboardEvent) {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    void submit();
  }
}

// Capability is earned once per CONVERSATION, not once per request: the tool
// ledger is the record of what this conversation loaded, so a skill loaded on
// an earlier turn — or in a conversation resumed from disk — keeps its gated
// tools unlocked. The backend re-checks this set at dispatch.
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
  // One active agent run at a time; Enter while running must not start a
  // second concurrent loop.
  if (isThinking.value) return;
  addMsg(text, "user");
  // The row just added is the CURRENT message: it travels in `message`, so
  // it must not also appear in the history.
  const currentId = nextId - 1;
  input.value = "";

  if (!llmSettings || !llmSettings.provider || !llmSettings.model) {
    addMsg("(no provider configured — open Settings to configure an LLM)", "dim");
    return;
  }

  // The whole transcript goes on the wire: every turn the model has seen,
  // minus the message just typed. No row cap — the backend windows by tokens
  // and cuts at turn boundaries (docs/COMPACTION.md), so a count here would
  // silently drop the user's task out from under the agent. The wire knows
  // user/assistant/summary: the tool ledger rides along as assistant context
  // ("🔧 name args -> result") so a resumed run knows what was already done,
  // and a compaction divider rides along as the summary that replaced the
  // rows above it.
  const priorHistory = messages.value
    .filter((m) => m.id !== currentId && isTranscriptRow(m))
    .map((m) => ({ role: m.cls === "tool" ? "assistant" : m.cls, content: m.text }));

  isThinking.value = true;
  liveMsgId = -1;

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
      // Compaction knobs: 0 = read the window off the model endpoint; the
      // percent is how much of it the agent may fill before summarizing.
      contextWindow: llmSettings.context_window ?? 0,
      compactAtPercent: llmSettings.compact_at_percent ?? 85,
    });
    // The return value is the unstreamed remainder (e.g. the turn-cap note
    // after a tool turn reset the live message). Only when nothing at all
    // came back is the reply truly empty.
    if (liveMsgId < 0) {
      if (final.trim()) addMsg(final, "assistant");
      else addMsg("(no response)", "dim");
    }
  } catch (e) {
    const msg = typeof e === "string" ? e : e && e.message ? e.message : String(e);
    addMsg(
      `Error: ${msg}\n\nThe agent stopped. For details, open firstmate.log in your temp folder (Windows: Win+R → %temp%).`,
      "error",
    );
  } finally {
    isThinking.value = false;
    liveMsgId = -1;
    persistConversation();
  }
}

// Ask the running agent loop to stop; it returns "(stopped)" at the next
// turn/tool boundary.
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

// Auto-scroll to bottom when new messages arrive. DynamicScroller
// measures item sizes asynchronously: right after a bulk load (opening
// a conversation, or the startup load while the window is still hidden)
// scrollHeight is a small estimate that momentarily "settles" at the
// estimated bottom long before real sizes land. So pin every frame
// until scrollHeight has been stable for a while AND we are at the
// bottom — touching the bottom once is not proof of arrival.
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
// A wheel event means the user is taking over the scroll position;
// stop pinning mid-settle rather than dragging them back down.
window.addEventListener("wheel", () => (userScrollAbort = true));

// Window-scoped shortcuts: Escape dismisses (wizard is Skip/Finish only),
// Ctrl+N starts a conversation, Ctrl+Shift+N a temporary one.
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
                <!-- Safe by construction: renderMd() = marked -> DOMPurify.sanitize. -->
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
              </DynamicScrollerItem>
            </template>
          </DynamicScroller>
          <div
            v-if="isThinking"
            class="msg dim thinking stoppable"
            title="Click to stop the agent"
            @click="stopRun"
          >
            First Mate {{ spinnerChar }} — click to stop
          </div>
          <div ref="inputRef" class="input-row">
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
      <SetupWizard v-if="wizardOpen" @done="wizardOpen = false" />
    </div>
  </n-config-provider>
</template>
