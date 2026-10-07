<script setup lang="ts">
import { getCurrentWindow } from "@tauri-apps/api/window";
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

function addMsg(text: string, cls = "") {
  messages.value.push({ id: nextId++, text, cls });
}

onMounted(() => {
  invoke<string>("get_hotkey").then((hotkey) => {
    addMsg(`First Mate is online. Summon with ${hotkey}.`, "dim");
  });
});

function submit() {
  const text = input.value.trim();
  if (!text) return;
  addMsg(text, "user");
  input.value = "";
  // LLM integration lands in the next phase; echo for now.
  addMsg(`(no provider wired yet) you said: "${text}"`, "dim");
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
      console.log("scrolling to bottom", el.scrollTop, el.scrollHeight);
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
      <div class="input-row">
        <n-input
          v-model:value="input"
          placeholder="Type a message…"
          @keydown.enter.prevent="submit"
        />
      </div>
    </div>
  </n-config-provider>
</template>
