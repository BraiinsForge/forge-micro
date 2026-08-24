import { render } from 'preact';
import './styles/common.css';
import './styles/index.css';
import App from './app';
import { initThemeAuto } from './lib/theme';

initThemeAuto();

// The mount point is a static element in index.html, so it is always
// present by the time this script runs.
render(<App />, document.getElementById('app')!);
