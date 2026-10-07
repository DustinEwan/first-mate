<script setup lang="ts">
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ref, onMounted, watch, nextTick } from "vue";
import { NConfigProvider, NInput, darkTheme } from "naive-ui";
import { DynamicScroller, DynamicScrollerItem } from "vue-virtual-scroller";
import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({ gfm: true, breaks: true, async: false });
// Model output is untrusted: sanitize before it reaches v-html.
function renderMd(text: string): string {
  return DOMPurify.sanitize(marked.parse(text));
}

const win = getCurrentWindow();

interface Msg {
  id: number;
  text: string;
  cls: string;
}

const messages = ref<Msg[]>([]);
const input = ref("");
let nextId = 0;
const scrollerRef = ref();
const isThinking = ref(false);
const inputRef = ref<HTMLElement>();
let liveMsgId = -1;
const spinnerChar = ref("⠋");

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

function addMsg(text: string, cls = "") {
  messages.value.push({ id: nextId++, text, cls });
}

onMounted(() => {
  invoke<string>("get_hotkey").then((hotkey) => {
    addMsg(`First Mate is online. Summon with ${hotkey}.`, "dim");
  });
  invoke<{ llm: LlmSettings }>("get_settings").then((s) => {
    llmSettings = s.llm;
  });
  invoke<SkillInfo[]>("list_skills").then((skills) => {
    if (skills.length === 0) return;
    const list = skills.map((s) => `- ${s.name}: ${s.description}`).join("\n");
    skillsAds.value =
      `\n\n## Skills (loaded on demand)\nAvailable skills:\n${list}\n` +
      `When a task matches a skill's domain, call load_skill("<name>") to get its ` +
      `full instructions before acting; read reference files with ` +
      `read_skill_resource("<skill>/<relative/path>").`;
  });
  // Focus the input when the window is shown/focused.
  nextTick(() => focusInput());
  listen("tauri://focus", () => focusInput());
  // Show tool calls in real-time as the agent makes them.
  listen<{ name: string; args: Record<string, string> }>("tool_call", (event) => {
    const { name, args } = event.payload;
    const argStr = Object.entries(args)
      .map(([k, v]) => `${k}=${v}`)
      .join(" ");
    addMsg(`🔧 ${name} ${argStr}`, "tool");
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


const SYSTEM_PROMPT = `You are First Mate, a Windows control agent. You can inspect and interact with running Windows applications using the winapp CLI.

## Available Tools
- run_command: Execute a command on the Windows system. Use this to run winapp commands.
- read_file: Read a file from the Windows filesystem.
- write_file: Write a file to the Windows filesystem.
- list_dir: List directory contents.

## WinApp UI Automation
Use winapp to inspect and interact with running Windows applications.

### List open windows (find app names / HWNDs)
\`\`\`
winapp ui list-windows
\`\`\`

### Inspect the UI tree (preferred; use --depth to limit size)
\`\`\`
winapp ui inspect -a <app-name> [--depth N]
winapp ui inspect <selector> -a <app-name>
\`\`\`

### Search for elements (slow, ~10s — prefer inspect with a selector)
\`\`\`
winapp ui search "<text>" -a <app-name>
\`\`\`

### Invoke (activate) an element
\`\`\`
winapp ui invoke <selector> -a <app-name>
\`\`\`

### Click an element
\`\`\`
winapp ui click <selector> -a <app-name>
\`\`\`

### Send keyboard input
\`\`\`
winapp ui send-keys ctrl+t -a <app-name>
winapp ui send-keys --target <selector> --via send-input --verbatim "literal text" -a <app-name>
winapp ui send-keys enter -a <app-name>
\`\`\`
Named keys (enter, tab, esc) and combos (ctrl+shift+t). To TYPE literal text
(search boxes, address bars), use --verbatim with --via send-input, then send
enter as a separate command.

### Set a value directly (often better than typing)
\`\`\`
winapp ui set-value <selector> <value> -a <app-name>
\`\`\`

### Screenshot (the image is attached to your context — you CAN see it)
\`\`\`
winapp ui screenshot -a <app-name>
\`\`\`
The screenshot image is attached automatically right after the command; look
at it to judge UI state, verify your last action, or find things the UIA tree
does not expose.

## Command Rules
- Quote multi-word arguments: winapp ui search "Qwen 3.8 Flash Next" -a zen
- Pipes, && and redirection work: tasklist | findstr /i zen
- There is NO 'winapp ui list' command — use 'winapp ui list-windows'.
- If a command errors with "was not matched", your syntax is wrong: run
  \`winapp ui <command> --help\` once, then use the exact syntax. Never re-guess.
- Element selectors go stale after the UI changes — re-inspect before clicking
  by slug, and never click a selector from an older inspect result twice.
- 'ui search' walks the whole UIA tree: 6-14 s on browsers/large apps, even
  with --root. Use it once for discovery only; afterwards navigate with
  'inspect <selector> --depth N' (~2 s) or just screenshot (~1 s, visible).

### Workflow
1. Use inspect (with --depth) to find element selectors
2. Use invoke/click/set-value/send-keys to interact
3. Re-inspect a small subtree to verify the result

When the user asks you to interact with a Windows application, use the run_command tool to execute winapp commands.

## Important: Convergence
- If a command fails, do NOT retry it more than once. Report the error to the user and suggest an alternative.
- Keep tool calls minimal: aim for 1-3 tool calls per task. Do not loop.
- When you have enough information to answer, stop calling tools and give your final text response.`;

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
  addMsg(text, "user");
  input.value = "";

  if (!llmSettings || !llmSettings.provider || !llmSettings.model) {
    addMsg("(no provider configured — open Settings to configure an LLM)", "dim");
    return;
  }

  // Send prior turns so the agent remembers earlier messages. The message
  // just added is the current one, so drop it from the history.
  const priorHistory = messages.value
    .filter((m) => (m.cls === "user" || m.cls === "assistant") && m.text.trim())
    .slice(0, -1)
    .map((m) => ({ role: m.cls, content: m.text }));

  isThinking.value = true;
  liveMsgId = -1;

  try {
    await invoke<string>("chat_with_llm", {
      provider: llmSettings.provider,
      baseUrl: llmSettings.base_url,
      apiKey: llmSettings.api_key,
      model: llmSettings.model,
      message: text,
      priorHistory,
      systemPrompt: SYSTEM_PROMPT + skillsAds.value,
    });
    // If no text ever streamed in (e.g. the model returned nothing), show the
    // final reply as a fallback.
    if (liveMsgId < 0) {
      addMsg("(no response)", "dim");
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
  }
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

// Escape hides the window.
window.addEventListener("keydown", (e) => {
  if (e.key === "Escape") hide();
});
</script>

<template>
  <n-config-provider :theme="darkTheme">
    <div class="chat">
      <header class="titlebar">
        <span class="title">First Mate</span>
        <button class="close" @click="hide">&#10005;</button>
      </header>
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
              v-if="item.cls === 'assistant'"
              class="msg assistant md"
              v-html="renderMd(item.text)"
            ></div>
            <div v-else class="msg" :class="item.cls">{{ item.text }}</div>
          </DynamicScrollerItem>
        </template>
      </DynamicScroller>
      <div v-if="isThinking" class="msg dim thinking">First Mate {{ spinnerChar }}</div>
      <div class="input-row" ref="inputRef">
        <n-input
          v-model:value="input"
          placeholder="Type a message…"
          @keydown.enter.prevent="submit"
        />
      </div>
    </div>
  </n-config-provider>
</template>
