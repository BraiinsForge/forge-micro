import ActionMenu from './ActionMenu';
import { cycleTheme } from '../lib/theme';
import type { BoardNotice } from '../app';

export interface TopNavProps {
  view: string;
  boardNotice: BoardNotice;
  /** The brand the board reports, which replaces the build-time one. */
  brand?: string | null;
}

export default function TopNav({ view, boardNotice, brand }: TopNavProps) {
  return (
    <nav class="bar">
      <span class="logo">
        <img src="/logo.svg" alt={brand || __BRAND_NAME__} />
      </span>
      <a href="#dash" class={view === 'dash' ? 'on' : ''}>
        Dashboard
      </a>
      <a href="#config" class={view === 'config' ? 'on' : ''}>
        Configuration
      </a>
      <a href="#events" class={view === 'events' ? 'on' : ''}>
        Events
      </a>
      <span class="sp"></span>

      <ActionMenu boardNotice={boardNotice} />

      <button
        type="button"
        class="thm"
        onClick={cycleTheme}
        aria-label="Cycle theme: auto, light, dark"
      >
        <svg class="thmIcon iAuto" aria-hidden="true">
          <use href="#iAuto" />
        </svg>
        <svg class="thmIcon iSun" aria-hidden="true">
          <use href="#iSun" />
        </svg>
        <svg class="thmIcon iMoon" aria-hidden="true">
          <use href="#iMoon" />
        </svg>
      </button>

      <form class="lo" method="post" action="/logout">
        <button id="nLogout" class="navBtn" type="submit" aria-label="Log out" title="Log out">
          <svg class="navIcon" aria-hidden="true">
            <use href="#iLogout" />
          </svg>
        </button>
      </form>
    </nav>
  );
}
