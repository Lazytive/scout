import { mount } from 'svelte';

import './app.css';
import Overlay from './Overlay.svelte';
import { blockBrowserUi } from './lib/browser-ui';

const target = document.getElementById('overlay');
if (!target) throw new Error('no #overlay element');

if ('__TAURI_INTERNALS__' in window) blockBrowserUi();

export default mount(Overlay, { target });
