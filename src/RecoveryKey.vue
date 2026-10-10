<script setup lang="ts">
// The key text lives only in the process that created it, so the parent must
// mount this view after the first store write.
import { invoke } from "@tauri-apps/api/core";
import { onMounted, ref } from "vue";
import { NAlert, NButton, NInput, NSpace } from "naive-ui";

const key = ref("");
const pending = ref(false);

onMounted(async () => {
  pending.value = await invoke<boolean>("recovery_key_status");
});

async function reveal() {
  key.value = (await invoke<string | null>("get_recovery_key")) ?? "";
}

async function ack() {
  await invoke("ack_recovery_key");
  pending.value = false;
  key.value = "";
}
</script>

<template>
  <n-alert v-if="pending" type="warning" :bordered="false" title="Recovery key">
    <div class="recovery">
      <p>
        First Mate seals every file it writes with a key your TPM holds. This recovery key opens
        those files after a TPM reset or a hardware loss.
      </p>
      <n-input v-if="key" :value="key" readonly />
      <p v-else>Choose Show key to read the recovery key once.</p>
      <n-space>
        <n-button v-if="!key" size="small" secondary @click="reveal">Show key</n-button>
        <n-button size="small" secondary @click="ack">I saved it</n-button>
      </n-space>
      <p class="dim">
        Save the key offline, outside this machine. First Mate forgets the text when you choose I
        saved it, and cannot show it again.
      </p>
    </div>
  </n-alert>
</template>

<style scoped>
.recovery {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.recovery p {
  margin: 0;
}
.dim {
  color: #8a8f98;
}
</style>
