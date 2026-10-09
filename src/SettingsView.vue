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
  NInputNumber,
  NButton,
  NCheckbox,
  NSpace,
  darkTheme,
} from "naive-ui";

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

const provider = ref("");
const baseUrl = ref("");
const apiKey = ref("");
const model = ref("");
// Compaction knobs. Nullable because the input can be cleared; a cleared or
// missing value means the default (0 = auto-detect, 85% of the window).
const contextWindow = ref<number | null>(0);
const compactAtPercent = ref<number | null>(85);
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
// Hosted providers hide Base URL unless the user opts into a proxy endpoint.
const proxy = ref(false);

const currentSpec = computed(() => providerSpecs.value.find((p) => p.id === provider.value));
const showBaseUrl = computed(
  () => !!currentSpec.value && (currentSpec.value.isLocal || proxy.value),
);

// Prefill endpoint + key hint when the user picks a provider. Fired only by
// the select, so loading saved settings never clobbers a custom base URL.
function onProviderChange(id: string) {
  const spec = providerSpecs.value.find((p) => p.id === id);
  if (!spec) return;
  baseUrl.value = spec.base;
  keyPlaceholder.value = spec.keyHint || "not required";
  proxy.value = false;
}

// Un-proxing restores the roster default so a stale proxy URL can't linger
// in the value that gets saved.
function onProxyChange(checked: boolean) {
  if (!checked && currentSpec.value) baseUrl.value = currentSpec.value.base;
}

// Include the current model in the options even if it's not in the discovered list.
function modelOptions() {
  const opts = models.value.map((m) => ({ label: m, value: m }));
  if (model.value && !models.value.includes(model.value)) {
    opts.push({ label: `${model.value} (not in list)`, value: model.value });
  }
  return opts;
}

// Load the saved settings into the form. Providers load first so the proxy
// state can be inferred: a hosted row saved with a non-default endpoint was
// configured through the proxy checkbox.
Promise.all([
  invoke<ProviderSpec[]>("list_providers"),
  invoke<{ llm: LlmSettings }>("get_settings"),
]).then(([ps, s]) => {
  providerSpecs.value = ps;
  providerOptions.value = [
    ...providerOptions.value,
    ...ps.map((p) => ({ label: p.label, value: p.id })),
  ];
  provider.value = s.llm.provider;
  baseUrl.value = s.llm.base_url;
  apiKey.value = s.llm.api_key;
  model.value = s.llm.model;
  contextWindow.value = s.llm.context_window ?? 0;
  compactAtPercent.value = s.llm.compact_at_percent ?? 85;
  const spec = ps.find((p) => p.id === s.llm.provider);
  proxy.value = !!spec && !spec.isLocal && s.llm.base_url !== "" && s.llm.base_url !== spec.base;
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
      context_window: contextWindow.value ?? 0,
      compact_at_percent: compactAtPercent.value ?? 85,
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
          <n-form-item v-if="currentSpec && !currentSpec.isLocal" :show-label="false">
            <n-checkbox v-model:checked="proxy" @update:checked="onProxyChange">Proxy</n-checkbox>
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
          <n-form-item label="Context window (tokens)">
            <n-input-number v-model:value="contextWindow" :min="0" :show-button="false" />
            <template #feedback>
              Detected from the model endpoint (max_model_len); set it when the endpoint reports
              nothing. 0 = auto-detect.
            </template>
          </n-form-item>
          <n-form-item label="Compact at % of context">
            <n-input-number
              v-model:value="compactAtPercent"
              :min="10"
              :max="100"
              :show-button="false"
            />
            <template #feedback>
              Compact the earlier turns once the conversation reaches this share of the context
              window (10–100, default 85).
            </template>
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
