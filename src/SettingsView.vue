<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { computed, ref } from "vue";
import {
  NConfigProvider,
  NForm,
  NFormItem,
  NSelect,
  NInput,
  NButton,
  NSpace,
  darkTheme,
} from "naive-ui";

interface LlmSettings {
  provider: string;
  model: string;
  api_key: string;
  base_url: string;
}

const provider = ref("");
const baseUrl = ref("");
const apiKey = ref("");
const model = ref("");
const models = ref<string[]>([]);
const status = ref("");

interface ProviderSpec {
  id: string;
  label: string;
  api: string;
  auth: string;
  base: string;
  keyHint: string;
  keyOptional: boolean;
  isLocal: boolean;
}

// The roster lives in the backend (providers.json); the UI renders it verbatim.
const providerSpecs = ref<ProviderSpec[]>([]);
const providerOptions = ref([{ label: "— none —", value: "" }]);
const keyPlaceholder = ref("sk-...");
invoke<ProviderSpec[]>("list_providers").then((ps) => {
  providerSpecs.value = ps;
  providerOptions.value = [
    ...providerOptions.value,
    ...ps.map((p) => ({ label: p.label, value: p.id })),
  ];
});

// Prefill endpoint + key hint when the user picks a provider. Fired only by
// the select, so loading saved settings never clobbers a custom base URL.
function onProviderChange(id: string) {
  const spec = providerSpecs.value.find((p) => p.id === id);
  if (!spec) return;
  baseUrl.value = spec.base;
  keyPlaceholder.value = spec.keyHint || "not required";
}

// Hosted endpoints are fixed by the roster; only user-owned ones (local
// engines, custom URLs) expose the Base URL field.
const showBaseUrl = computed(
  () => providerSpecs.value.find((p) => p.id === provider.value)?.isLocal ?? false,
);

// Include the current model in the options even if it's not in the discovered list.
function modelOptions() {
  const opts = models.value.map((m) => ({ label: m, value: m }));
  if (model.value && !models.value.includes(model.value)) {
    opts.push({ label: `${model.value} (not in list)`, value: model.value });
  }
  return opts;
}

// Load the saved settings into the form.
invoke<{ llm: LlmSettings }>("get_settings").then((s) => {
  provider.value = s.llm.provider;
  baseUrl.value = s.llm.base_url;
  apiKey.value = s.llm.api_key;
  model.value = s.llm.model;
});

async function refreshModels() {
  status.value = "Refreshing models…";
  try {
    models.value = await invoke<string[]>("list_models", {
      provider: provider.value,
      baseUrl: baseUrl.value,
      apiKey: apiKey.value,
    });
    status.value =
      models.value.length > 0 ? `${models.value.length} models found` : "no models found";
  } catch (e) {
    status.value = `Error: ${e}`;
  }
}

async function save() {
  const settings = {
    llm: {
      provider: provider.value,
      model: model.value,
      api_key: apiKey.value,
      base_url: baseUrl.value,
    },
  };
  try {
    await invoke("save_settings", { settings });
    status.value = "Saved";
    setTimeout(() => (status.value = ""), 2000);
  } catch (e) {
    status.value = `Error: ${e}`;
  }
}

async function test() {
  status.value = "Testing connection…";
  try {
    status.value = await invoke<string>("test_llm", {
      provider: provider.value,
      baseUrl: baseUrl.value,
      apiKey: apiKey.value,
      model: model.value,
    });
  } catch (e) {
    status.value = `Test failed: ${e}`;
  }
}

const win = getCurrentWindow();
function closeWindow() {
  console.log("closeWindow() called");
  win
    .close()
    .then(() => console.log("closed"))
    .catch((e) => console.error("close error", e));
}
</script>

<template>
  <n-config-provider :theme="darkTheme">
    <div class="settings">
      <header class="titlebar">
        <span class="title">&#9881; First Mate Settings</span>
        <button class="close" @click="closeWindow">&#10005;</button>
      </header>
      <main class="content">
        <n-form label-placement="top" :show-require-mark="false">
          <n-form-item label="Provider">
            <n-select
              v-model:value="provider"
              :options="providerOptions"
              @update:value="onProviderChange"
            />
          </n-form-item>
          <n-form-item v-if="showBaseUrl" label="Base URL">
            <n-input v-model:value="baseUrl" placeholder="e.g. http://localhost:11434" />
          </n-form-item>
          <n-form-item label="API key">
            <n-input
              v-model:value="apiKey"
              type="password"
              show-password-on="click"
              :placeholder="keyPlaceholder"
            />
          </n-form-item>
          <n-form-item label="Model">
            <n-space align="center" :wrap="false">
              <n-select
                v-model:value="model"
                :options="modelOptions()"
                placeholder="— none —"
                clearable
              />
              <n-button size="small" secondary @click="refreshModels"> &#8635; refresh </n-button>
            </n-space>
          </n-form-item>
        </n-form>
        <n-space align="center">
          <n-button type="primary" @click="save">Save</n-button>
          <n-button @click="test">Test</n-button>
        </n-space>
        <div v-if="status" class="status">{{ status }}</div>
      </main>
    </div>
  </n-config-provider>
</template>
