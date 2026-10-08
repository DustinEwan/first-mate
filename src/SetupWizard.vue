<script setup lang="ts">
// First-run stepper: model setup and harness readiness, MUI-Stepper style
// via Naive UI's n-steps. Shown when no model is configured; reopenable
// from the tray ("Setup Wizard").
import { invoke } from "@tauri-apps/api/core";
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import {
  NAlert,
  NCheckbox,
  NButton,
  NConfigProvider,
  NForm,
  NFormItem,
  NInput,
  NSelect,
  NSpace,
  NStep,
  NSteps,
  darkTheme,
} from "naive-ui";

const emit = defineEmits<{ done: [] }>();

const step = ref(1);
const hotkey = ref("");

// --- Step 2: model -------------------------------------------------------
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
const modelStatus = ref("");

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
const providersReady = invoke<ProviderSpec[]>("list_providers").then((ps) => {
  providerSpecs.value = ps;
  providerOptions.value = [
    ...providerOptions.value,
    ...ps.map((p) => ({ label: p.label, value: p.id })),
  ];
  return ps;
});

const currentSpec = computed(() => providerSpecs.value.find((p) => p.id === provider.value));

// Picking a provider prefills its endpoint (the watch below then auto-
// discovers models) and its key hint. Select-only: restored settings stay.
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

// Hosted endpoints are fixed by the roster; only user-owned ones (local
// engines, custom URLs) or an explicit proxy override expose Base URL.
const showBaseUrl = computed(
  () => !!currentSpec.value && (currentSpec.value.isLocal || proxy.value),
);

function modelOptions() {
  const opts = models.value.map((m) => ({ label: m, value: m }));
  if (model.value && !models.value.includes(model.value)) {
    opts.push({ label: `${model.value} (not in list)`, value: model.value });
  }
  return opts;
}

async function refreshModels() {
  modelStatus.value = "Refreshing models…";
  try {
    models.value = await invoke<string[]>("list_models", {
      provider: provider.value,
      baseUrl: baseUrl.value,
      apiKey: apiKey.value,
    });
    // Populate the dropdown with something: preselect the first discovery
    // so the step is complete on arrival; the user may override.
    if (!model.value && models.value.length > 0) {
      model.value = models.value[0];
    }
    modelStatus.value =
      models.value.length > 0 ? `${models.value.length} models found` : "no models found";
  } catch (e) {
    modelStatus.value = `Error: ${e}`;
  }
}

// Auto-detect: once provider + base URL are both present, discover models
// shortly after typing settles (debounced so mid-URL keystrokes don't fire).
let detectTimer: ReturnType<typeof setTimeout> | null = null;
watch([provider, baseUrl], ([p, u]) => {
  if (detectTimer) clearTimeout(detectTimer);
  if (!p || !u) return;
  detectTimer = setTimeout(refreshModels, 700);
});

const modelReady = () => provider.value !== "" && model.value !== "";

async function saveModel() {
  const settings = {
    llm: {
      provider: provider.value,
      model: model.value,
      api_key: apiKey.value,
      base_url: baseUrl.value,
    },
  };
  await invoke("save_settings", { settings });
}

// --- Step 3: harness ------------------------------------------------------
interface BootstrapStatus {
  winapp: string | null;
  winget: boolean;
}
const boot = ref<BootstrapStatus | null>(null);
const installing = ref(false);
let poll: ReturnType<typeof setInterval> | null = null;

async function checkHarness() {
  boot.value = await invoke<BootstrapStatus>("bootstrap_status");
}

function openDocs() {
  invoke("open_path", {
    target: "https://learn.microsoft.com/windows/apps/dev-tools/winapp-cli/",
  });
}

async function installWinapp() {
  installing.value = true;
  try {
    await invoke<string>("install_winapp");
    // Poll until the CLI answers; the install itself announces in the ledger.
    poll = setInterval(async () => {
      await checkHarness();
      if (boot.value?.winapp) {
        if (poll) clearInterval(poll);
        poll = null;
        installing.value = false;
      }
    }, 3000);
  } catch (e) {
    modelStatus.value = `Install failed: ${e}`;
    installing.value = false;
  }
}

async function nextFromModel() {
  await saveModel();
  step.value = 3;
  checkHarness();
}

onMounted(async () => {
  hotkey.value = await invoke<string>("get_hotkey");
  const [ps, s] = await Promise.all([providersReady, invoke<{ llm: LlmSettings }>("get_settings")]);
  provider.value = s.llm.provider;
  baseUrl.value = s.llm.base_url;
  apiKey.value = s.llm.api_key;
  model.value = s.llm.model;
  // A hosted row saved with a non-default endpoint was proxied.
  const spec = ps.find((p) => p.id === s.llm.provider);
  proxy.value = !!spec && !spec.isLocal && s.llm.base_url !== "" && s.llm.base_url !== spec.base;
  if (provider.value && baseUrl.value) refreshModels();
});

onBeforeUnmount(() => {
  if (poll) clearInterval(poll);
});
</script>

<template>
  <n-config-provider :theme="darkTheme">
    <div class="wizard">
      <div class="wizard-card">
        <n-steps :current="step" size="small">
          <n-step title="Welcome" />
          <n-step title="Model" />
          <n-step title="Harness" />
          <n-step title="Ready" />
        </n-steps>

        <div v-if="step === 1" class="wizard-body">
          <h2>⚓ First Mate</h2>
          <p>
            A local-first agent that controls this machine: shell, files, and real desktop apps via
            the winapp CLI. Summon it from anywhere with
            <b>{{ hotkey }}</b
            >.
          </p>
          <p class="dim">
            Setup takes about a minute: pick a model, then verify the desktop harness.
          </p>
        </div>

        <div v-else-if="step === 2" class="wizard-body">
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
              <div class="model-row">
                <n-select
                  v-model:value="model"
                  :options="modelOptions()"
                  placeholder="— none —"
                  clearable
                />
                <n-button size="small" secondary :disabled="provider === ''" @click="refreshModels">
                  &#8635; refresh
                </n-button>
              </div>
            </n-form-item>
          </n-form>
          <div v-if="modelStatus" class="status">{{ modelStatus }}</div>
        </div>

        <div v-else-if="step === 3" class="wizard-body">
          <p>
            Desktop automation runs through the <b>winapp</b> CLI — Microsoft's official tool for
            inspecting and driving Windows apps. First Mate checks for it on every start but never
            installs anything without asking.
          </p>
          <n-alert v-if="boot === null" type="default" :bordered="false">Checking harness…</n-alert>
          <n-alert v-else-if="boot.winapp" type="success" :bordered="false">
            winapp {{ boot.winapp }} is installed — UI automation is ready.
          </n-alert>
          <template v-else>
            <n-alert type="warning" :bordered="false">
              The winapp CLI is not installed, so First Mate can't drive desktop apps yet.
            </n-alert>
            <n-alert v-if="!boot.winget" type="error" :bordered="false">
              winget is unavailable on this machine, so First Mate can't install it. Install the
              winapp CLI manually, then reopen this wizard.
            </n-alert>
            <div v-else class="install-row">
              <n-button
                type="primary"
                :loading="installing"
                :disabled="installing"
                @click="installWinapp"
              >
                {{ installing ? "Installing…" : "Install winapp CLI" }}
              </n-button>
              <n-button text tag="a" @click="openDocs">What am I installing?</n-button>
            </div>
            <p v-if="installing" class="dim">
              Installing via winget — this pane updates when it lands.
            </p>
          </template>
        </div>

        <div v-else class="wizard-body">
          <h2>You're set</h2>
          <p>
            First Mate can now run commands, edit files, and drive your desktop apps using
            <b>{{ model }}</b
            >.
          </p>
          <p>
            Summon it with <b>{{ hotkey }}</b> and just ask — "screenshot the browser", "commit my
            changes", "what's eating my CPU".
          </p>
        </div>

        <div class="wizard-foot">
          <n-button text @click="emit('done')">Skip setup</n-button>
          <n-space>
            <n-button v-if="step > 1" secondary @click="step--">Back</n-button>
            <n-button v-if="step === 1" type="primary" @click="step = 2">Next</n-button>
            <n-button
              v-else-if="step === 2"
              type="primary"
              :disabled="!modelReady()"
              @click="nextFromModel"
              >Save &amp; continue</n-button
            >
            <n-button
              v-else-if="step === 3"
              type="primary"
              :disabled="!boot?.winapp"
              @click="step = 4"
              >Next</n-button
            >
            <n-button v-else type="primary" @click="emit('done')">Finish</n-button>
          </n-space>
        </div>
      </div>
    </div>
  </n-config-provider>
</template>

<style scoped>
.wizard {
  position: absolute;
  inset: 0;
  z-index: 30;
  background: rgba(16, 17, 19, 0.92);
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 16px;
}
.wizard-card {
  width: 100%;
  max-width: 560px;
  background: #18181c;
  border: 1px solid #2c2c31;
  border-radius: 10px;
  padding: 20px 24px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  max-height: 100%;
  overflow: auto;
}
.wizard-body h2 {
  margin: 0 0 8px;
  font-size: 18px;
}
.wizard-body p {
  margin: 0 0 8px;
  line-height: 1.5;
  color: #c9ced6;
}
.wizard-body .dim {
  color: #8a8f98;
}
.status {
  color: #8a8f98;
  font-size: 13px;
  margin-top: 4px;
}
.model-row {
  display: flex;
  gap: 8px;
  width: 100%;
  align-items: center;
}
.model-row .n-select {
  flex: 1;
  min-width: 0;
}
.install-row {
  display: flex;
  align-items: center;
  gap: 16px;
}
.wizard-body .n-alert + .install-row,
.wizard-body .n-alert + .n-alert {
  margin-top: 16px;
}
.wizard-foot {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
</style>
