// Scout's setup window (ADR 0023): the page apps/klip-setup opens (setup.html).
import { mount } from 'svelte';

import './app.css';
import Setup from './Setup.svelte';
import { blockBrowserUi } from './lib/browser-ui';

const target = document.getElementById('setup');
if (!target) throw new Error('no #setup element');

if ('__TAURI_INTERNALS__' in window) blockBrowserUi();

export default mount(Setup, { target });
