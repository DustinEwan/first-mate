<script setup lang="ts">
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ref, onMounted, watch, nextTick } from "vue";
import { NConfigProvider, NInput, darkTheme } from "naive-ui";
import { DynamicScroller, DynamicScrollerItem } from "vue-virtual-scroller";

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

### Inspect the UI tree
\`\`\`
winapp ui inspect -a <app-name>
\`\`\`

### Search for elements
\`\`\`
winapp ui search <selector> -a <app-name>
\`\`\`

### Invoke (activate) an element
\`\`\`
winapp ui invoke <selector> -a <app-name>
\`\`\`

### Click an element
\`\`\`
winapp ui click <selector> -a <app-name>
\`\`\`

### Take a screenshot
\`\`\`
winapp ui screenshot -a <app-name>
\`\`\`

### Send keyboard input
\`\`\`
winapp ui send-keys <keys> -a <app-name>
\`\`\`

### Set a value
\`\`\`
winapp ui set-value <selector> <value> -a <app-name>
\`\`\`

### Workflow
1. Start with a screenshot to see the current state
2. Use inspect to find element selectors
3. Use invoke/click/set-value to interact
4. Use screenshot again to verify the result

When the user asks you to interact with a Windows application, use the run_command tool to execute winapp commands.

## Important: Convergence
- If a command fails, do NOT retry it more than once. Report the error to the user and suggest an alternative.
- Keep tool calls minimal: aim for 1-3 tool calls per task. Do not loop.
- When you have enough information to answer, stop calling tools and give your final text response.`;
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
      systemPrompt: SYSTEM_PROMPT,
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
            <div class="msg" :class="item.cls">{{ item.text }}</div>
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
