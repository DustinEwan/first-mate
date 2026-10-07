<script setup lang="ts">
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { ref, onMounted, watch, nextTick } from "vue";
import { NConfigProvider, NInput, darkTheme } from "naive-ui";
import { RecycleScroller } from "vue-virtual-scroller";

const win = getCurrentWindow();

interface Msg {
  id: number;
  text: string;
  cls: string;
}

const messages = ref<Msg[]>([]);
const input = ref("");
let nextId = 0;
const messagesWrap = ref<HTMLElement>();
const isThinking = ref(false);
const inputRef = ref<HTMLElement>();

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

When the user asks you to interact with a Windows application, use the run_command tool to execute winapp commands.`;
async function submit() {
  const text = input.value.trim();
  if (!text) return;
  addMsg(text, "user");
  input.value = "";

  if (!llmSettings || !llmSettings.provider || !llmSettings.model) {
    addMsg("(no provider configured — open Settings to configure an LLM)", "dim");
    return;
  }

  isThinking.value = true;
  addMsg("…", "dim");
  const thinkingId = nextId - 1;

  try {
    const reply = await invoke<string>("chat_with_llm", {
      provider: llmSettings.provider,
      baseUrl: llmSettings.base_url,
      apiKey: llmSettings.api_key,
      model: llmSettings.model,
      message: text,
      systemPrompt: SYSTEM_PROMPT,
    });
    // Replace the "…" placeholder with the actual response.
    const idx = messages.value.findIndex((m) => m.id === thinkingId);
    if (idx >= 0) {
      messages.value[idx] = { id: thinkingId, text: reply, cls: "assistant" };
    } else {
      addMsg(reply, "assistant");
    }
  } catch (e) {
    const idx = messages.value.findIndex((m) => m.id === thinkingId);
    if (idx >= 0) {
      messages.value[idx] = { id: thinkingId, text: `Error: ${e}`, cls: "error" };
    } else {
      addMsg(`Error: ${e}`, "error");
    }
  } finally {
    isThinking.value = false;
  }
}


function hide() {
  console.log("hide() called");
  win.hide().then(() => console.log("hidden")).catch((e) => console.error("hide error", e));
}

// Auto-scroll to bottom when new messages arrive.
watch(messages, () => {
  nextTick(() => {
    const el = messagesWrap.value;
    if (el) {
      // Only auto-scroll if the user is already near the bottom (following the conversation).
      const nearBottom = el.scrollTop + el.clientHeight >= el.scrollHeight - 80;
      if (nearBottom) {
        el.scrollTop = el.scrollHeight;
      }
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
      <div class="messages-wrap" ref="messagesWrap">
        <RecycleScroller
          :items="messages"
          :item-size="72"
          key-field="id"
          :page-mode="true"
        >
          <template #default="{ item }">
            <div class="msg" :class="item.cls">{{ item.text }}</div>
          </template>
        </RecycleScroller>
      </div>
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
