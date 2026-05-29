import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";

export default tseslint.config(
  {
    ignores: [
      "dist/**",
      "node_modules/**",
      "src-tauri/**",
      "landing/**",
      "vite.config.ts",
      "eslint.config.js",
    ],
  },
  js.configs.recommended,
  // Type-aware linting for src/. projectService auto-discovers files (works for tests too).
  {
    files: ["src/**/*.{ts,tsx}"],
    extends: [
      ...tseslint.configs.strictTypeChecked,
      ...tseslint.configs.stylisticTypeChecked,
    ],
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
      // New in react-hooks v7. Useful signal but flags legitimate "reset+fetch on open" patterns;
      // downgrade to warn so it's visible but not blocking.
      "react-hooks/set-state-in-effect": "warn",
      // The codebase uses `type X = {...}` consistently. Don't fight it.
      "@typescript-eslint/consistent-type-definitions": ["error", "type"],
      // `onClick={() => setX(true)}` is idiomatic React — setX returns void but reads fine.
      "@typescript-eslint/no-confusing-void-expression": [
        "error",
        { ignoreArrowShorthand: true },
      ],
      // Async event handlers in JSX are common; the parent doesn't await them anyway.
      "@typescript-eslint/no-misused-promises": [
        "error",
        { checksVoidReturn: { attributes: false } },
      ],
      // `${count}` where count is a number is fine — that's what template literals are for.
      "@typescript-eslint/restrict-template-expressions": [
        "error",
        { allowNumber: true, allowBoolean: true },
      ],
      // `delete map[key]` is the idiomatic toggle pattern on Record<string, T>. logic.ts uses it.
      "@typescript-eslint/no-dynamic-delete": "off",
    },
  },
  {
    files: ["src/**/*.test.{ts,tsx}"],
    rules: {
      "@typescript-eslint/no-non-null-assertion": "off",
      "@typescript-eslint/no-unsafe-assignment": "off",
      "@typescript-eslint/no-unsafe-member-access": "off",
      "@typescript-eslint/non-nullable-type-assertion-style": "off",
    },
  },
);
