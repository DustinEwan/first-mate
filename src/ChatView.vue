<script setup lang="ts">
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
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

interface Msg {
  id: number;
  text: string;
  cls: string;
  // Tool ledger bookkeeping: which tool the line belongs to, and whether
  // its result has landed (pending lines get replaced by the result block).
  toolName?: string;
  done?: boolean;
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
}
const panelOpen = ref(false);
const conversations = ref<ConvInfo[]>([]);
const convName = ref<string | null>(null);

function focusInput() {
  const el = inputRef.value?.querySelector("input");
  if (el) el.focus();
}

interface LlmSettings {
  provider: string;
  model: string;
  api_key: string;
  base_url: string;
}
let llmSettings: LlmSettings | null = null;

function addMsg(text: string, cls = "", extra: Partial<Msg> = {}) {
  messages.value.push({ id: nextId++, text, cls, ...extra });
}

function persistConversation() {
  // The tool ledger persists too: a resumed conversation can see what was
  // already inspected, verified, and executed instead of re-doing it.
  const turns = messages.value
    .filter((m) => ["user", "assistant", "tool"].includes(m.cls) && m.text.trim())
    .map((m) => ({ role: m.cls, text: m.text }));
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
      messages.value = msgs.map((m) => ({ id: nextId++, text: m.text, cls: m.role }));
      convName.value = c.name;
      panelOpen.value = false;
    })
    .catch((e) => console.error("load failed", e));
}

function newChat() {
  messages.value = [];
  convName.value = null;
  panelOpen.value = false;
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
  });
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
      `read_skill_resource("<skill>/<relative/path>"). Loading a skill may also ` +
      `enable additional tools for the rest of the conversation.`;
  });
  // Focus the input when the window is shown/focused.
  nextTick(() => focusInput());
  listen("tauri://focus", () => focusInput());
  // Show tool calls in real-time as the agent makes them.
  listen<{ name: string; args: Record<string, string>; identity?: string }>(
    "tool_call",
    (event) => {
      const { name, args, identity } = event.payload;
      const fallback = Object.entries(args)
        .map(([k, v]) => `${k}=${String(v).slice(0, 40)}`)
        .join(" ");
      addMsg(`🔧 ${identity ?? `${name} ${fallback}`}`, "tool", { toolName: name });
    },
  );
  // Replace the pending line with the backend's ledger block:
  // identity / fenced effect / metadata. The block persists to .chat and
  // resumes as context, so it states what happened, not what was asked.
  listen<{ name: string; summary: string }>("tool_result", (event) => {
    const { name, summary } = event.payload;
    let matched = false;
    for (let i = messages.value.length - 1; i >= 0; i--) {
      const m = messages.value[i];
      if (m.cls === "tool" && m.toolName === name && !m.done) {
        messages.value[i] = { ...m, text: `🔧 ${summary}`, done: true };
        matched = true;
        break;
      }
    }
    // Unmatched results are async announcements (e.g. a background job that
    // finished after its turn): they get their own ledger line so the model
    // resumes with them as context.
    if (!matched) addMsg(`🔧 ${summary}`, "tool", { toolName: name, done: true });
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
      messages.value[idx] = { id: liveMsgId, text: messages.value[idx].text + event.payload.text, cls: "assistant" };
    }
  });
  // A tool-call turn: finalize the current live message so the next turn
  // streams into a fresh one.
  listen("stream_reset", () => {
    liveMsgId = -1;
  });
});


// Behavior text comes from AGENTS.md (repo root or ~/.firstmate), loaded via
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
async function submit() {
  const text = input.value.trim();
  if (!text) return;
  // One active agent run at a time; Enter while running must not start a
  // second concurrent loop.
  if (isThinking.value) return;
  addMsg(text, "user");
  input.value = "";

  if (!llmSettings || !llmSettings.provider || !llmSettings.model) {
    addMsg("(no provider configured — open Settings to configure an LLM)", "dim");
    return;
  }

  // Send prior turns so the agent remembers recent context. The message
  // just added is the current one, so drop it from the history. Windowed:
  // an unbounded history bloats every request (760KB observed) and, worse,
  // conditions small models into narrating without ever calling tools.
  const priorHistory = messages.value
    .filter((m) => ["user", "assistant", "tool"].includes(m.cls) && m.text.trim())
    .slice(0, -1)
    .slice(-30)
    // The wire only knows user/assistant: the tool ledger rides along as
    // assistant context ("🔧 name args -> result") so a resumed run knows
    // what was already done. Tools themselves stay undisclosed until reloaded.
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
    });
    // The return value is the unstreamed remainder (e.g. the turn-cap note
    // after a tool turn reset the live message). Only when nothing at all
    // came back is the reply truly empty.
    if (liveMsgId < 0) {
      if (final.trim()) addMsg(final, "assistant");
      else addMsg("(no response)", "dim");
    }
  } catch (e) {
    const msg = typeof e === "string" ? e : (e && e.message) ? e.message : String(e);
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
  win.hide().then(() => console.log("hidden")).catch((e) => console.error("hide error", e));
}

// Auto-scroll to bottom when new messages arrive.
watch(messages, () => {
  nextTick(() => {
    const el = scrollerRef.value?.$el as HTMLElement | undefined;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  });
}, { deep: true });

// Escape closes the panel first, then hides the window.
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (panelOpen.value) panelOpen.value = false;
    else hide();
  }
});
</script>

<template>
  <n-config-provider :theme="darkTheme">
    <div class="chat">
      <header class="titlebar">
        <span class="title">⚓️ First Mate</span>
        <button
          class="panel-toggle"
          :title="panelOpen ? 'Hide chats' : 'Show chats'"
          @click="panelOpen = !panelOpen; refreshConversations()"
        >&#9776;</button>
        <button class="close" @click="hide">&#10005;</button>
      </header>
      <div class="body">
        <div class="main">
          <DynamicScroller
            ref="scrollerRef"
            :items="messages"
            :min-size="40"
            key-field="id"
            class="scroller"
          >
            <template v-slot="{ item, active }">
              <DynamicScrollerItem
                :item="item"
                :active="active"
                :size-dependencies="[item.text]"
              >
                <div
                  v-if="item.cls === 'assistant' || item.cls === 'tool'"
                  class="msg md"
                  :class="item.cls"
                  @click="onContentClick"
                  v-html="renderMd(item.text)"
                ></div>
                <div v-else class="msg" :class="item.cls">{{ item.text }}</div>
              </DynamicScrollerItem>
            </template>
          </DynamicScroller>
          <div
            v-if="isThinking"
            class="msg dim thinking stoppable"
            title="Click to stop the agent"
            @click="stopRun"
          >First Mate {{ spinnerChar }} — click to stop</div>
          <div class="input-row" ref="inputRef">
            <n-input
              v-model:value="input"
              placeholder="Type a message…"
              @keydown.enter.prevent="submit"
            />
          </div>
        </div>
        <aside v-if="panelOpen" class="side">
          <div class="side-head">
            <span>Chats</span>
            <button class="new-chat" @click="newChat">New</button>
          </div>
          <ul class="conv-list">
            <li
              v-for="c in conversations"
              :key="c.path"
              :class="{ active: c.name === convName }"
              :title="c.name"
              @click="openConversation(c)"
            >{{ c.name }}</li>
          </ul>
        </aside>
      </div>
    </div>
  </n-config-provider>
</template>
