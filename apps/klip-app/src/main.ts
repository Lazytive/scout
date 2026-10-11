import { mount } from 'svelte';

import './app.css';
import App from './App.svelte';
import { restoreLook } from './lib/appearance';
import { blockBrowserUi } from './lib/browser-ui';

const target = document.getElementById('app');
if (!target) throw new Error('no #app element');

// In the app the WebView's Print / Save as / Inspect are not Scout's (the browser mock
// keeps them for working on the UI).
if ('__TAURI_INTERNALS__' in window) blockBrowserUi();

// The last look before the first paint (a custom theme doesn't flash Dark).
restoreLook();

export default mount(App, { target });
