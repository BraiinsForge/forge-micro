// Login page bootstrap — no framework. Pulls in the shared and
// login-specific styles (Vite inlines them into the single-file page)
// and reveals the error banner. Two entry cases raise it: the server
// bounces a failed attempt back with ?err=1 (default "invalid
// credentials" text), and the SPA redirects here with ?expired=1 after
// a 401, in which case the banner text is swapped for a session notice.
import './styles/common.css';
import './styles/login.css';
import { tabTitle } from './lib/title';

const params = new URLSearchParams(location.search);
const err = document.getElementById('err');
// The banner is a static element in login.html, but guard rather than
// assert: this script has no non-null way to prove it at compile time.
if (err) {
  if (params.has('expired')) {
    err.textContent = 'Your session expired. Please sign in again.';
    err.classList.add('show');
  } else if (params.has('err')) {
    err.classList.add('show');
  }
}

// This page cannot poll for the board's address before sign-in, so
// the host from the address bar stands in.
document.title = tabTitle();
