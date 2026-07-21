// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

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
