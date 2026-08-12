// Theme cycler. The boot script in each page's <head> seeded
// data-theme-pref and data-theme before first paint; this advances the
// preference auto -> light -> dark on each click and persists it. Which
// cycler icon shows is driven purely by the data-theme-pref attribute
// in CSS, so no reactive state is needed here.

const THEME_PREFS = ['auto', 'light', 'dark'];
const prefersLight = matchMedia('(prefers-color-scheme: light)');

function applyThemePref(pref: string): void {
  document.documentElement.dataset.themePref = pref;
  const resolved = pref === 'auto' ? (prefersLight.matches ? 'light' : 'dark') : pref;
  document.documentElement.dataset.theme = resolved;
}

export function cycleTheme(): void {
  const current = document.documentElement.dataset.themePref || 'auto';
  const next = THEME_PREFS[(THEME_PREFS.indexOf(current) + 1) % THEME_PREFS.length];
  try {
    localStorage.setItem('theme', next);
  } catch (_) {
    /* private mode; keep the in-page preference */
  }
  applyThemePref(next);
}

// Re-resolve the palette when the OS flips prefers-color-scheme, but
// only while the preference is auto so an explicit choice is never
// overruled.
export function initThemeAuto(): void {
  prefersLight.addEventListener('change', () => {
    if ((document.documentElement.dataset.themePref || 'auto') === 'auto') {
      applyThemePref('auto');
    }
  });
}
