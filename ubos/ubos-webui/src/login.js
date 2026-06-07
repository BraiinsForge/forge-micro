// Surface the "Invalid username or password" banner when the server
// bounced us back here with ?err=1. The login endpoint sets that
// query parameter on auth failure and redirects so the password
// never lands in the browser history as a POST body.
const params = new URLSearchParams(location.search);
if (params.get('err')) {
  document.getElementById('err').classList.add('show');
}

// Theme toggle. The boot script in <head> set the initial value on
// <html data-theme>; this handler flips it on click and persists the
// choice in localStorage so the next page load skips the system
// fallback.
document.getElementById('thm').addEventListener('click', () => {
  const current = document.documentElement.dataset.theme === 'light' ? 'light' : 'dark';
  const next = current === 'light' ? 'dark' : 'light';
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem('theme', next); } catch (_) { }
});
