import js from '@eslint/js';
import ts from '@typescript-eslint/eslint-plugin';
import tsParser from '@typescript-eslint/parser';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';

export default [
  js.configs.recommended,

  // TypeScript source files
  {
    files: ['**/*.ts'],
    languageOptions: {
      parser: tsParser,
      globals: globals.browser,
    },
    plugins: { '@typescript-eslint': ts },
    rules: {
      ...ts.configs.recommended.rules,
      '@typescript-eslint/no-explicit-any': 'warn',
      '@typescript-eslint/consistent-type-imports': ['error', { prefer: 'type-imports' }],
    },
  },

  // Svelte files — spread the full recommended array so its overrides take effect
  // (including no-self-assign: 'off' for the arr = arr reactivity trigger pattern)
  ...svelte.configs['flat/recommended'],

  // Add TypeScript parser inside Svelte scripts + project-specific rule tuning
  {
    files: ['**/*.svelte'],
    languageOptions: {
      globals: globals.browser,
      parserOptions: { parser: tsParser },
    },
    plugins: { '@typescript-eslint': ts },
    rules: {
      '@typescript-eslint/consistent-type-imports': ['error', { prefer: 'type-imports' }],
      // Underscore prefix = intentionally unused; ignoreRestSiblings for `const { x, ...rest }`
      'no-unused-vars': ['error', { ignoreRestSiblings: true, argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],
      // All {@html} in this project renders internal SVG icon strings, not user content
      'svelte/no-at-html-tags': 'warn',
      // Tauri SPA uses adapter-static with fallback — resolve() is not applicable
      'svelte/no-navigation-without-resolve': 'off',
    },
  },

  {
    ignores: ['.svelte-kit/', 'node_modules/'],
  },
];
