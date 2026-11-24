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
import { fmtHr } from './lib/format';
import { API, getSystemInfo, loginRedirect, sleep } from './lib/api';
import { tabTitle } from './lib/title';
import type { SystemInfo } from './lib/types';

// Hash-based view switch, no router. `#config[/sub]` -> config,
// `#events` -> events, anything else -> dashboard.
function viewOf(hash: string): 'config' | 'events' | 'dash' {
  return hash.startsWith('#config') ? 'config' : hash === '#events' ? 'events' : 'dash';
}

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

  // The brand and product names come from the board's stored identity,
  // which does not change while it runs, so they are read once. A failed
  // read leaves the build-time brand and no product name.
  const [hw, setHw] = useState<SystemInfo['hwInfo'] | null>(null);

  useEffect(() => {
    startPoll();
    getSystemInfo().then(
      (info) => setHw(info.hwInfo),
      () => {},
    );
    const onHash = () => setHash(location.hash);
    addEventListener('hashchange', onHash);
    return () => removeEventListener('hashchange', onHash);
  }, []);

  // Name the tab after the board it shows, so a row of open tabs tells
  // one board from the next. The address comes from the poll and not
  // from the address bar, which holds an mDNS name or a proxy address on
  // some networks. Until the first answer the tab carries the address
  // bar's host, which is what the login page shows.
  const ip = dash.info?.ip;
  const brand = hw?.brandName;
  useEffect(() => {
    document.title = tabTitle(ip, brand);
  }, [ip, brand]);

  return (
    <>
      {/* Shared SVG symbol library reached by <use href="#..."> everywhere. */}
      <div dangerouslySetInnerHTML={{ __html: icons }} />

      <TopNav view={view} boardNotice={boardNotice} brand={brand} />

      <main id="dash" class={view !== 'dash' ? 'hide' : ''}>
        <Dashboard shown={view === 'dash'} />
      </main>

      {view === 'config' && <Config hash={hash} boardNotice={boardNotice} />}

      {view === 'events' && <Events />}

      {/* The product name and the nominal hashrate lead, so an operator
          can match the page to the sticker on the machine. */}
      <footer class="bar ft">
        <span class="bi">
          {hw?.productName ? hw.productName + ' · ' : ''}
          {dash.info?.nominal_hashrate ? fmtHr(dash.info.nominal_hashrate) + ' · ' : ''}
          IP: {dash.info?.ip || '–'} · MAC: {dash.info?.mac || '–'} · {dash.info?.build || '–'}
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
