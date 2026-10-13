import React, { useCallback, useEffect, useState } from 'react';
import ReactDOM from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createRouter } from '@tanstack/react-router';
import { routeTree } from './routes/routeTree';
import { settingsGetAll, settingsSet, isBackendNotReadyError } from './lib/tauri';
import { openNoteTab } from './lib/openNote';
import { installExternalLinkGuard } from './lib/externalLinks';
import { OnboardingWizard } from './components/layout/OnboardingWizard';
import { PetBallWindow } from './components/pet/PetBallWindow';
import { PetBubbleWindow } from './components/pet/PetBubbleWindow';
import { PetMenuWindow } from './components/pet/PetMenuWindow';
import { PetChatWindow } from './components/pet/PetChatWindow';
import { NoteWindow } from './components/notes/NoteWindow';
import { getCurrentWindow } from '@tauri-apps/api/window';
import 'katex/dist/katex.min.css';
import 'pdfjs-dist/legacy/web/pdf_viewer.css';
import './index.css';

// App-wide link guard, installed before anything renders: a click on a link
// that leaves the app opens in the system browser instead of navigating the
// webview and replacing the UI. This runs in every window (main, pet, note).
installExternalLinkGuard();

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      // Startup race backstop: AppState is manage()d only after async DB init,
      // so any query fired before that fails with "state not managed". Keep
      // retrying those until the backend is up (they always self-heal); all
      // other errors keep the normal budget. This also covers windows/paths
      // that bypass the backendReady gate (secondary windows, late mounts).
      retry: (failureCount, error) =>
        isBackendNotReadyError(error) || failureCount < 2,
      retryDelay: (attempt, error) =>
        isBackendNotReadyError(error) ? 500 : Math.min(1000 * 2 ** attempt, 30_000),
    },
  },
});

const router = createRouter({ routeTree });
(window as Window & { __sikuRouter?: typeof router }).__sikuRouter = router;

declare module '@tanstack/react-router' {
  interface Register { router: typeof router; }
}

// ============================================================
// Startup performance tracking
// ============================================================
interface StartupMetric { phase: string; timestamp: number; elapsed_ms: number; }
const startupMetrics: StartupMetric[] = [];
const T0 = performance.now();

function trackPhase(phase: string) {
  const now = performance.now();
  startupMetrics.push({ phase, timestamp: now, elapsed_ms: Math.round(now - T0) });
}

async function flushStartupMetrics() {
  try {
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('log_startup_metrics', {
      metrics: startupMetrics.map((m) => ({
        phase: m.phase,
        elapsed_ms: m.elapsed_ms,
      })),
    });
  } catch { /* best-effort */ }
}

// ============================================================
// App
// ============================================================

const ONBOARDING_KEY = 'siku.onboarding.completed';

function markOnboardingCompletedLocal() {
  try {
    localStorage.setItem(ONBOARDING_KEY, '1');
  } catch { /* best-effort */ }
}

function App() {
  const [showOnboarding, setShowOnboarding] = useState(false);
  // Route-mounted queries (papers/collections/tags) fire the moment the
  // router renders. Backend init (DB migrations, crsqlite load, token
  // refresh, auto-sync proxy) takes seconds, and react-query's retry: 2
  // (~3s of backoff) often expires before AppState is managed — those
  // queries settle in error and the collection tree shows "暂无集合" until
  // a later sync:remote_applied invalidation rescues it. Gate the router on
  // backend readiness so first queries succeed on the first try.
  const [backendReady, setBackendReady] = useState(false);
  // Set when backend init outlasts the fast probe window: the main window is
  // shown with an "initializing" placeholder instead of the splash, but the
  // router stays unmounted until the backend actually answers.
  const [initStalled, setInitStalled] = useState(false);

  React.useEffect(() => {
    trackPhase('react_mounted');

    // Deep link: another window requested this note ("open in new window").
    try {
      const raw = localStorage.getItem('siku.pending-note');
      if (raw) {
        localStorage.removeItem('siku.pending-note');
        const parsed = JSON.parse(raw) as { id?: string; ts?: number };
        if (parsed.id && typeof parsed.ts === 'number' && Date.now() - parsed.ts < 30_000) {
          const router = (window as Window & { __sikuRouter?: { navigate: (o: { to: string; search?: Record<string, unknown> }) => Promise<unknown> } }).__sikuRouter;
          if (router) {
            openNoteTab((opts) => router.navigate(opts).catch(() => {}), { id: parsed.id });
          }
        }
      }
    } catch { /* ignore */ }

    // Safety: if backend init outlasts the fast probe window (slow machine,
    // first-run migrations), show the main window with an "initializing"
    // placeholder instead of leaving the user staring at the splash — but do
    // NOT mount the router: its queries would race AppState registration and
    // fail with "state not managed". The slow probe below still flips
    // backendReady once the backend actually answers.
    const safetyTimer = setTimeout(() => {
      import('@tauri-apps/api/window').then(({ getCurrentWindow, Window }) => {
        getCurrentWindow().show().catch(() => {});
        Window.getByLabel('splashscreen')
          .then((splash) => splash?.close())
          .catch(() => {});
      }).catch(() => {});
      setInitStalled(true);
    }, 10_000);

    let cancelled = false;
    (async () => {
      // Load persisted settings. The onboarding completion flag lives in the
      // database — a fresh DB (first install, or after deleting the data
      // directory) never has it, so the wizard shows again. We intentionally
      // do NOT gate on "settings table is empty": the backend writes
      // infrastructure settings (e.g. notes.current_vault_id, fts.rebuilt)
      // during startup, so an empty-table check is unreliable.
      //
      // Poll until the backend answers: a failed probe means AppState is not
      // managed yet (init still running), not that the backend is dead. Fast
      // probes for ~10s, then 1s probes indefinitely — a slow machine or a
      // large migration must never push the UI past this gate.
      let onboardingCompletedInDb = false;
      for (let attempt = 0; !cancelled; attempt++) {
        try {
          const entries = await settingsGetAll();
          onboardingCompletedInDb = entries.some((e) => e.key === ONBOARDING_KEY && e.value === '1');
          break;
        } catch {
          await new Promise((r) => setTimeout(r, attempt < 40 ? 250 : 1000));
        }
      }
      if (cancelled) return;
      setBackendReady(true);

      trackPhase('settings_loaded');

      // Signal to backend that frontend is ready.
      // Once both frontend and backend are done, the splashscreen
      // window closes and the main window becomes visible.
      try {
        const { invoke } = await import('@tauri-apps/api/core');
        await invoke('set_complete');
        clearTimeout(safetyTimer);
      } catch { /* ignore — app may run outside Tauri */ }

      trackPhase('frontend_ready');

      // Background update check (no-op in dev, silent on failure).
      import('./lib/updater').then(({ checkForUpdatesOnStartup }) => checkForUpdatesOnStartup()).catch(() => {});

      // Show the wizard unless completion is recorded in the database. (The
      // probe above only ends on a successful read, so the flag is always
      // authoritative here.)
      if (!onboardingCompletedInDb) {
        setTimeout(() => setShowOnboarding(true), 300);
      }

      // Flush metrics in background
      flushStartupMetrics();
    })();

    return () => {
      cancelled = true;
      clearTimeout(safetyTimer);
    };
  }, []);

  // Only a real finish (or "不再显示") marks onboarding complete. Esc/✕
  // dismisses for this launch only, so a user who never configured an LLM
  // keeps getting the wizard instead of a broken first experience.
  const handleOnboardingDone = useCallback((markComplete: boolean) => {
    if (markComplete) {
      markOnboardingCompletedLocal();
      settingsSet(ONBOARDING_KEY, '1').catch(() => {});
    }
    setShowOnboarding(false);
  }, []);

  // Settings → 通用 → 重新打开引导 fires this event.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    import('@tauri-apps/api/event')
      .then(({ listen }) => listen('app:show-onboarding', () => setShowOnboarding(true)))
      .then((fn) => { unlisten = fn; })
      .catch(() => {});
    return () => unlisten?.();
  }, []);

  return (
    <QueryClientProvider client={queryClient}>
      {/* Router mounts only after the backend answers — see backendReady. */}
      {backendReady && <RouterProvider router={router} />}
      {!backendReady && initStalled && (
        <div style={{
          display: 'flex', flexDirection: 'column', alignItems: 'center',
          justifyContent: 'center', height: '100vh', background: '#1A1A1E',
          color: '#F5F5F5', fontFamily: 'sans-serif', gap: 8,
        }}>
          <div>正在初始化数据库…</div>
          <div style={{ fontSize: 12, color: '#9CA3AF' }}>
            首次启动或数据较多时可能需要更长时间
          </div>
        </div>
      )}
      {showOnboarding && (
        <OnboardingWizard onDone={handleOnboardingDone} />
      )}
    </QueryClientProvider>
  );
}

// Error boundary — prevents white/black screen on crash
class ErrorFallback extends React.Component<
  { children: React.ReactNode },
  { hasError: boolean; error: Error | null }
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { hasError: false, error: null };
  }
  static getDerivedStateFromError(error: Error) {
    return { hasError: true, error };
  }
  render() {
    if (this.state.hasError) {
      // Surface the error instead of hiding it behind the splashscreen:
      // show the main window AND close the splash. The splash is normally
      // closed by the backend (try_finish_startup) once both sides are
      // ready — on a render crash that never happens, so the error UI
      // would otherwise stay covered by the splash forever.
      // (No fixed timeout involved: slow machines are never affected.)
      import('@tauri-apps/api/window').then(({ getCurrentWindow, Window }) => {
        getCurrentWindow().show().catch(() => {});
        Window.getByLabel('splashscreen')
          .then((splash) => splash?.close())
          .catch(() => {});
      }).catch(() => {});
      return (
        <div style={{
          display: 'flex', flexDirection: 'column', alignItems: 'center',
          justifyContent: 'center', height: '100vh', background: '#1A1A1E',
          color: '#F5F5F5', fontFamily: 'sans-serif', padding: 40,
        }}>
          <h1 style={{ color: '#E67E22', marginBottom: 16 }}>思库</h1>
          <p style={{ color: '#9CA3AF', marginBottom: 24 }}>应用加载失败，请重启。</p>
          <pre style={{ color: '#E74C3C', fontSize: 12, maxWidth: 480, overflow: 'auto' }}>
            {this.state.error?.message}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}

// In the Tauri window the body must be transparent so the CSS rounded
// corners on the app shell let the desktop show through.
if (typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window) {
  document.body.classList.add('in-tauri');
}

// Window-specific entry points: the pet window renders only the floating
// ball, pet-bubble the ephemeral speech bubble, pet-chat windows the
// popped-out conversation, note windows just the opened note, everything
// else is the app.
let windowKind: 'app' | 'pet' | 'pet-bubble' | 'pet-menu' | 'pet-chat' | 'note' = 'app';
try {
  const label = getCurrentWindow().label;
  if (label === 'pet') windowKind = 'pet';
  else if (label === 'pet-bubble') windowKind = 'pet-bubble';
  else if (label === 'pet-menu') windowKind = 'pet-menu';
  else if (label.startsWith('pet-chat-')) windowKind = 'pet-chat';
  else if (label.startsWith('note-')) windowKind = 'note';
} catch {
  windowKind = 'app';
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  windowKind === 'pet' ? (
    <PetBallWindow />
  ) : windowKind === 'pet-bubble' ? (
    <PetBubbleWindow />
  ) : windowKind === 'pet-menu' ? (
    <PetMenuWindow />
  ) : windowKind === 'pet-chat' ? (
    <PetChatWindow />
  ) : windowKind === 'note' ? (
    <NoteWindow />
  ) : (
    <ErrorFallback>
      <App />
    </ErrorFallback>
  ),
);
