import { mount } from 'svelte';

import './app.css';
import Overlay from './Overlay.svelte';

const target = document.getElementById('overlay');
if (!target) throw new Error('no #overlay element');

export default mount(Overlay, { target });
