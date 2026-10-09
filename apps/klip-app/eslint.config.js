import js from '@eslint/js';
import svelte from 'eslint-plugin-svelte';
import { defineConfig } from 'eslint/config';
import globals from 'globals';
import ts from 'typescript-eslint';

import svelteConfig from './svelte.config.js';

export default defineConfig(
  { ignores: ['dist/', 'src-tauri/', 'node_modules/'] },
  js.configs.recommended,
  ...ts.configs.recommended,
  ...svelte.configs.recommended,
  { languageOptions: { globals: { ...globals.browser } } },
  {
    files: ['**/*.svelte', '**/*.svelte.ts'],
    languageOptions: {
      parserOptions: {
        projectService: true,
        extraFileExtensions: ['.svelte'],
        parser: ts.parser,
        svelteConfig,
      },
    },
  },
);
