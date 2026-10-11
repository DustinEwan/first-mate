<script setup lang="ts">
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { computed, ref, watch } from "vue";
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
import RecoveryKey from "./RecoveryKey.vue";

interface LlmSettings {
  provider: string;
  model: string;
  api_key: string;
  base_url: string;
  // docs/COMPACTION.md explains the compaction design.
  context_window?: number;
  compact_at_percent?: number;
}

const provider = ref("");
const baseUrl = ref("");
const apiKey = ref("");
const model = ref("");
const contextWindow = ref<number | null>(0);
const compactAtPercent = ref<number | null>(85);
const hideSensitive = ref<Partial<Record<string, boolean>>>({});
const decisionUrl = ref("");
const decisionEnabled = ref(false);
const decisionThreshold = ref<number | null>(0.5);
const decisionRemoteOk = ref(false);
const decisionHost = ref("");
const decisionRemote = ref(false);
const models = ref<string[]>([]);
const status = ref("");
const activeTab = ref("model");

interface ProviderSpec {
  id: string;
  label: string;
  api: string;
  auth: string;
  base: string;
  keyHint: string;
  keyOptional: boolean;
  isLocal: boolean;
  hideSensitiveDefault: boolean;
}

const providerSpecs = ref<ProviderSpec[]>([]);
const providerOptions = ref([{ label: "— none —", value: "" }]);
const keyPlaceholder = ref("sk-...");
const proxy = ref(false);

const currentSpec = computed(() => providerSpecs.value.find((p) => p.id === provider.value));
const showBaseUrl = computed(
  () => !!currentSpec.value && (currentSpec.value.isLocal || proxy.value),
);

const hideSensitiveDefault = computed(() => currentSpec.value?.hideSensitiveDefault ?? true);
const hideSensitiveChecked = computed({
  get: () => hideSensitive.value[provider.value] ?? hideSensitiveDefault.value,
  set: (on: boolean) => {
    if (provider.value) hideSensitive.value[provider.value] = on;
  },
});

// Only the select calls this, so loading saved settings never overwrites
// a custom base URL.
function onProviderChange(id: string) {
  const spec = providerSpecs.value.find((p) => p.id === id);
  if (!spec) return;
  baseUrl.value = spec.base;
  keyPlaceholder.value = spec.keyHint || "not required";
  proxy.value = false;
}

// A cleared Proxy must not leave a proxy URL in the saved settings.
function onProxyChange(checked: boolean) {
  if (!checked && currentSpec.value) baseUrl.value = currentSpec.value.base;
}

// A saved model may be absent from discovery. Keep that model selectable.
function modelOptions() {
  const opts = models.value.map((m) => ({ label: m, value: m }));
  if (model.value && !models.value.includes(model.value)) {
    opts.push({ label: `${model.value} (not in list)`, value: model.value });
  }
  return opts;
}

// Proxy state needs the provider roster: a hosted provider with a
// non-default saved endpoint means Proxy was on.
async function load() {
  const [ps, s] = await Promise.all([
    invoke<ProviderSpec[]>("list_providers"),
    invoke<{
      llm: LlmSettings;
      hide_sensitive: Record<string, boolean>;
      decision_server_url?: string;
      decision_server_enabled?: boolean;
      decision_threshold?: number;
      decision_server_remote_ok?: boolean;
    }>("get_settings"),
  ]);
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
  hideSensitive.value = s.hide_sensitive;
  decisionUrl.value = s.decision_server_url ?? "";
  decisionEnabled.value = !!s.decision_server_enabled;
  decisionThreshold.value = s.decision_threshold ?? 0.5;
  decisionRemoteOk.value = !!s.decision_server_remote_ok;
  void checkHost(s.decision_server_url ?? "");
  const spec = ps.find((p) => p.id === s.llm.provider);
  proxy.value = !!spec && !spec.isLocal && s.llm.base_url !== "" && s.llm.base_url !== spec.base;
}

void load();

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
    hide_sensitive: hideSensitive.value,
    decision_server_url: decisionUrl.value,
    decision_server_enabled: decisionEnabled.value,
    decision_threshold: decisionThreshold.value ?? 0.5,
    decision_server_remote_ok: decisionRemoteOk.value,
  };
  try {
    await invoke("save_settings", { settings });
    status.value = "Saved";
    setTimeout(() => (status.value = ""), 2000);
  } catch (e) {
    status.value = `Error: ${e}`;
  }
}

// The ✕ button hides the window, so an unsaved edit survives a reopen.
// Cancel re-reads the saved file before it closes.
async function cancel() {
  await load();
  closeWindow();
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

// A server that answers at its floor flags every span, so the probe refuses
// that server.
async function probeServer() {
  status.value = "Asking the second model…";
  try {
    const p = await invoke<{ benign: number; credential: number; usable: boolean }>(
      "probe_decision_server",
      { url: decisionUrl.value },
    );
    const benign = p.benign.toFixed(2);
    const credential = p.credential.toFixed(2);
    status.value = p.usable
      ? `ordinary text ${benign}, text with a password ${credential}`
      : `ordinary text ${benign}, text with a password ${credential}: this model cannot tell the two apart`;
  } catch (e) {
    status.value = `The second model did not answer: ${e}`;
  }
}

// Rust owns the loopback rule, so the warning and the gate cannot disagree.
async function checkHost(url: string) {
  if (url.trim() === "") {
    decisionHost.value = "";
    decisionRemote.value = false;
    return;
  }
  const h = await invoke<{ host: string; loopback: boolean }>("decision_host", { url });
  decisionHost.value = h.host;
  decisionRemote.value = !h.loopback;
}

let hostTimer: ReturnType<typeof setTimeout> | null = null;
watch(decisionUrl, (url) => {
  if (hostTimer) clearTimeout(hostTimer);
  hostTimer = setTimeout(() => void checkHost(url), 200);
});

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
        <recovery-key />

        <!-- naive-ui tabs set the pane wrapper's max-height from JavaScript, so a
             naive-ui pane cannot scroll. -->
        <div class="cats">
          <nav class="rail">
            <button :class="{ on: activeTab === 'model' }" @click="activeTab = 'model'">
              Model
            </button>
            <button :class="{ on: activeTab === 'privacy' }" @click="activeTab = 'privacy'">
              Privacy
            </button>
          </nav>
          <section class="pane-scroll">
            <n-form v-if="activeTab === 'model'" label-placement="top" :show-require-mark="false">
              <n-form-item label="Provider">
                <n-select
                  v-model:value="provider"
                  :options="providerOptions"
                  @update:value="onProviderChange"
                />
              </n-form-item>
              <n-form-item v-if="currentSpec && !currentSpec.isLocal" :show-label="false">
                <n-checkbox v-model:checked="proxy" @update:checked="onProxyChange">
                  Proxy
                </n-checkbox>
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
                  <n-button size="small" secondary @click="refreshModels">
                    &#8635; refresh
                  </n-button>
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
              <n-form-item :show-label="false">
                <n-button @click="test">Test</n-button>
                <template #feedback> Sends one short request to the model above.</template>
              </n-form-item>
            </n-form>

            <n-form v-else label-placement="top" :show-require-mark="false">
              <n-form-item :show-label="false">
                <n-checkbox v-model:checked="hideSensitiveChecked">
                  Hide sensitive information
                </n-checkbox>
                <template #feedback>
                  Applies to {{ currentSpec ? currentSpec.label : provider }} only. First Mate hides
                  card numbers, account numbers, passwords, and file paths from the model service.
                  Your saved conversation keeps the real values.
                </template>
              </n-form-item>
              <n-form-item :show-label="false">
                <n-checkbox v-model:checked="decisionEnabled">
                  Look for other sensitive information
                </n-checkbox>
                <template #feedback>
                  Hide sensitive information catches only card numbers, account numbers, passwords,
                  and file paths. A second model you run reads your text and looks for other
                  sensitive information. First Mate hides what that model finds. Your saved
                  conversation keeps the real values. First Mate never installs or starts that
                  model.
                </template>
              </n-form-item>
              <n-form-item v-if="decisionEnabled" label="Second model address">
                <n-space align="center" :wrap="false">
                  <n-input v-model:value="decisionUrl" placeholder="e.g. http://127.0.0.1:8741" />
                  <n-button size="small" secondary @click="probeServer">Test</n-button>
                </n-space>
                <template #feedback>
                  First Mate sends the second model only the words it needs checked, never your
                  whole conversation. Test sends one ordinary sentence and one sentence with a
                  password. The two scores should be far apart.
                </template>
              </n-form-item>
              <n-form-item v-if="decisionEnabled && decisionRemote" :show-label="false">
                <n-checkbox v-model:checked="decisionRemoteOk">
                  Let {{ decisionHost }} read the words First Mate checks
                </n-checkbox>
                <template #feedback>
                  {{ decisionHost }} is not on this PC. That computer keeps what it reads. Without
                  this box, First Mate hides only card numbers, account numbers, passwords, and file
                  paths.
                </template>
              </n-form-item>
              <n-form-item v-if="decisionEnabled" label="Second model must be this sure">
                <n-input-number
                  v-model:value="decisionThreshold"
                  :min="0"
                  :max="1"
                  :step="0.05"
                  :show-button="false"
                />
                <template #feedback>
                  The model must reach this level before First Mate hides anything. A higher level
                  hides less. Test shows two scores. Pick a level between them. Default 0.5.
                </template>
              </n-form-item>
            </n-form>
          </section>
        </div>

        <n-space align="center" class="footer">
          <n-button type="primary" @click="save">Save</n-button>
          <n-button @click="cancel">Cancel</n-button>
          <span v-if="status" class="status">{{ status }}</span>
        </n-space>
      </main>
    </div>
  </n-config-provider>
</template>
