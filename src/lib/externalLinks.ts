import { open } from '@tauri-apps/plugin-shell';

/** Where an anchor activation belongs. */
export type LinkRoute =
  /** The app itself: `#` destinations, relative routes, same-origin URLs. */
  | 'app'
  /** Another application's job: http(s), mailto:, tel:. */
  | 'external'
  /** Inline content the app renders itself (image previews): leave it alone. */
  | 'inline'
  /** A scheme we refuse to follow (`javascript:`, `file:`, unknown). */
  | 'blocked';

/** Route a link by scheme instead of by luck.
 *
 *  Every anchor in this app ends up here, so one table decides what happens to
 *  a paper's DOI, a note's markdown link, an attachment thumbnail and a
 *  hand-written `javascript:` alike. */
export function routeLink(href: string): LinkRoute {
  const raw = href.trim();
  if (!raw || raw.startsWith('#')) return 'app';

  // A schemeless address is a route inside the app. It cannot be resolved
  // against the base URL here: in release builds the app is served from
  // `tauri://localhost`, which is not a hierarchical URL, so `new URL()` has
  // nothing to resolve against.
  if (!/^[a-z][a-z0-9+.-]*:/i.test(raw)) return 'app';

  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return 'blocked';
  }

  // Same-origin URLs are the app's own (the Vite server in dev, the tauri:
  // scheme in release builds).
  if (url.origin === window.location.origin || url.protocol === 'tauri:') return 'app';

  switch (url.protocol) {
    case 'http:':
    case 'https:':
    case 'mailto:':
    case 'tel:':
      return 'external';
    case 'data:':
    case 'blob:':
    case 'about:':
      return 'inline';
    default:
      return 'blocked';
  }
}

/** Hand a link to the system browser.
 *
 *  A navigation inside the webview would replace the whole app view — no tab
 *  bar, no way back but a restart — so external addresses never navigate. */
export function openInBrowser(url: string): void {
  open(url).catch(() => {
    // Fallback when running outside Tauri (browser dev/tests).
    window.open(url, '_blank', 'noopener');
  });
}

/** Act on one anchor activation. Returns true when the event must be consumed
 *  (and therefore also stopped from reaching the app's own click handling). */
export function activateLink(href: string): boolean {
  switch (routeLink(href)) {
    case 'external':
      openInBrowser(href);
      return true;
    case 'blocked':
      // Swallow it: a refused scheme must not navigate the webview either.
      return true;
    default:
      return false;
  }
}

function closestAnchor(target: EventTarget | null): HTMLAnchorElement | null {
  const el = target as HTMLElement | null;
  if (!el || typeof el.closest !== 'function') return null;
  return el.closest('a[href]');
}

/** App-wide safety net, installed once by the shell: any click on a link that
 *  leaves the app opens in the system browser instead of replacing the UI.
 *
 *  Capture phase, so it runs before the readers' own click handlers (the PDF
 *  pane reports bare clicks for comparison anchoring) and can stop them.
 *  Returns a disposer. */
export function installExternalLinkGuard(): () => void {
  const onActivate = (e: MouseEvent) => {
    // Middle-click is a browser convention for "open elsewhere"; the left
    // button is the common case.
    if (e.type === 'auxclick' && e.button !== 1) return;
    const anchor = closestAnchor(e.target);
    if (!anchor) return;
    const href = anchor.getAttribute('href') ?? '';
    if (!activateLink(href)) return;
    e.preventDefault();
    e.stopPropagation();
  };

  document.addEventListener('click', onActivate, true);
  document.addEventListener('auxclick', onActivate, true);
  return () => {
    document.removeEventListener('click', onActivate, true);
    document.removeEventListener('auxclick', onActivate, true);
  };
}
