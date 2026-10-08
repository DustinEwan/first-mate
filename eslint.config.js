import pluginVue from "eslint-plugin-vue";
import vueTsEslintConfig from "@vue/eslint-config-typescript";
import skipFormatting from "@vue/eslint-config-prettier/skip-formatting";

export default [
  { name: "app/files-to-ignore", ignores: ["dist/**", "node_modules/**", "src-tauri/target/**"] },
  ...pluginVue.configs["flat/recommended"],
  ...vueTsEslintConfig(),
  skipFormatting,
  {
    rules: {
      // Ledger lines are intentionally terse; keep the Vue-isms that fight it out.
      "vue/multi-word-component-names": "off",
      "vue/singleline-html-element-content-newline": "off",
      "vue/max-attributes-per-line": "off",
      "vue/html-self-closing": "off",
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_" }],
    },
  },
];
