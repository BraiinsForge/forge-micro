// Theme cycler. The boot script in <head> seeded data-theme-pref and
// data-theme; this handler advances the preference auto -> light ->
// dark -> auto on each click and persists the new choice. The
// matchMedia listener re-applies the palette when the OS flips
// prefers-color-scheme, but only while the preference is auto so an
// explicit light or dark choice is never overruled.
const THEME_PREFS = ['auto', 'light', 'dark'];
const prefersLight = matchMedia('(prefers-color-scheme: light)');

function applyThemePref(pref) {
  document.documentElement.dataset.themePref = pref;
  const resolved = pref === 'auto' ? (prefersLight.matches ? 'light' : 'dark') : pref;
  document.documentElement.dataset.theme = resolved;
}

// The login page drops the footer that hosts the toggle, so the
// button is absent there; only wire the click handler when present.
const thm = document.getElementById('thm');
if (thm) {
  thm.addEventListener('click', () => {
    const current = document.documentElement.dataset.themePref || 'auto';
    const next = THEME_PREFS[(THEME_PREFS.indexOf(current) + 1) % THEME_PREFS.length];
    try { localStorage.setItem('theme', next); } catch (_) { }
    applyThemePref(next);
  });
}

prefersLight.addEventListener('change', () => {
  if ((document.documentElement.dataset.themePref || 'auto') === 'auto') {
    applyThemePref('auto');
  }
});
