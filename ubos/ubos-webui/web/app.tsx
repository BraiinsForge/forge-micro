import { useEffect, useState } from 'preact/hooks';
// Shared SVG symbol library, imported verbatim (`?raw`) and injected
// once via dangerouslySetInnerHTML below. Each icon is a <symbol> with
// its own viewBox, reached through a use-href; new icons go in
// icons.svg. Keep that file comment-free: `?raw` ships it byte for byte
// into the bundle, so a comment there bloats every page.
import icons from './icons.svg?raw';
import TopNav from './components/TopNav';
import Dashboard from './components/Dashboard';
import Config from './components/Config';
import Events from './components/Events';
import NetOverlays from './components/NetOverlays';
import Toasts from './components/Toasts';
import { startPoll, useDash } from './lib/store';
import { API, loginRedirect, sleep } from './lib/api';

// Hash-based view switch, no router. `#config[/sub]` -> config,
// `#events` -> events, anything else -> dashboard.
function viewOf(hash: string): 'config' | 'events' | 'dash' {
  return hash.startsWith('#config') ? 'config' : hash === '#events' ? 'events' : 'dash';
}

function formatBuildTime(raw: string): string {
  const seconds = Number(raw);
  if (!seconds) return '';
  return new Date(seconds * 1000).toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
}

// Build provenance injected by Vite `define` (see vite.config.ts).
const buildRev = __BUILD_REV__;
const buildTime = formatBuildTime(__BUILD_TIME__);

interface Notice {
  title: string;
  text: string;
}

// Shared with TopNav and ActionMenu, which both take this as a prop:
// show the board-unavailable notice, optionally polling until it comes
// back.
export type BoardNotice = (title: string, text: string, poll?: boolean) => Promise<void>;

export default function App() {
  const dash = useDash();
  const [hash, setHash] = useState(location.hash);
  const view = viewOf(hash);

  // Full-page board-unavailable overlay for the restart flow.
  const [notice, setNotice] = useState<Notice | null>(null);

  // Show the board-unavailable notice; when `poll` is set, poll
  // /api/info until the board answers, then reload.
  const boardNotice: BoardNotice = async (title, text, poll) => {
    setNotice({ title, text });
    if (!poll) return;
    for (let attempt = 0; attempt < 150; attempt++) {
      await sleep(2000);
      try {
        const response = await fetch(API + '/api/info', { cache: 'no-store' });
        // A reboot clears the in-RAM sessions, so the board answers the
        // recovery poll with 401 once it is back. Send the operator to
        // login rather than polling until the attempt cap.
        if (response.status === 401) {
          loginRedirect();
          return;
        }
        if (response.ok) {
          location.reload();
          return;
        }
      } catch (_) {
        /* still down; keep waiting */
      }
    }
  };

  useEffect(() => {
    startPoll();
    const onHash = () => setHash(location.hash);
    addEventListener('hashchange', onHash);
    return () => removeEventListener('hashchange', onHash);
  }, []);

  return (
    <>
      {/* Shared SVG symbol library reached by <use href="#..."> everywhere. */}
      <div dangerouslySetInnerHTML={{ __html: icons }} />

      <TopNav view={view} boardNotice={boardNotice} />

      <main id="dash" class={view !== 'dash' ? 'hide' : ''}>
        <Dashboard />
      </main>

      {view === 'config' && <Config hash={hash} boardNotice={boardNotice} />}

      {view === 'events' && <Events />}

      <footer class="bar ft">
        <span class="bi">
          IP: {dash.info?.ip || '–'} · MAC: {dash.info?.mac || '–'} · {buildRev} ·{' '}
          {buildTime}
        </span>
      </footer>

      {notice && (
        <div class="fullNotice">
          <div>
            <h1>{notice.title}</h1>
            <p>{notice.text}</p>
          </div>
        </div>
      )}

      <NetOverlays />

      <Toasts />
    </>
  );
}
