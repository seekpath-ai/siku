import React, { useCallback, useState } from 'react';
import ReactDOM from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createRouter } from '@tanstack/react-router';
import { routeTree } from './routes/routeTree';
import { settingsGetAll, settingsSet } from './lib/tauri';
import { openNoteTab } from './lib/openNote';
import { OnboardingWizard } from './components/layout/OnboardingWizard';
import { PetBallWindow } from './components/pet/PetBallWindow';
import { PetChatWindow } from './components/pet/PetChatWindow';
import { NoteWindow } from './components/notes/NoteWindow';
import { getCurrentWindow } from '@tauri-apps/api/window';
import 'katex/dist/katex.min.css';
import 'pdfjs-dist/legacy/web/pdf_viewer.css';
import './index.css';

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 30_000, retry: 2 },
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

function isOnboardingCompletedLocal(): boolean {
  try {
    return localStorage.getItem(ONBOARDING_KEY) === '1';
  } catch {
    return false;
  }
}

function markOnboardingCompletedLocal() {
  try {
    localStorage.setItem(ONBOARDING_KEY, '1');
  } catch { /* best-effort */ }
}

function App() {
  const [showOnboarding, setShowOnboarding] = useState(false);

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

    // Safety: if backend never responds (e.g. panic), show main
    // window after 10s so the user isn't stuck with a splash forever.
    const safetyTimer = setTimeout(() => {
      import('@tauri-apps/api/window').then(({ getCurrentWindow }) => {
        getCurrentWindow().show().catch(() => {});
      }).catch(() => {});
    }, 10_000);

    let cancelled = false;
    (async () => {
      let onboardingCompletedInDb = false;
      let backendOk = true;

      // Load persisted settings. The onboarding completion flag lives in the
      // database — a fresh DB (first install, or after deleting the data
      // directory) never has it, so the wizard shows again. We intentionally
      // do NOT gate on "settings table is empty": the backend writes
      // infrastructure settings (e.g. notes.current_vault_id, fts.rebuilt)
      // during startup, so an empty-table check is unreliable.
      try {
        const entries = await settingsGetAll();
        onboardingCompletedInDb = entries.some((e) => e.key === ONBOARDING_KEY && e.value === '1');
      } catch {
        backendOk = false;
      }
      if (cancelled) return;

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

      // Show the wizard unless completion is recorded in the database.
      // localStorage is only consulted as a fallback when the backend is
      // unreachable (a stale WebView2 flag must not suppress the wizard
      // after the database was wiped).
      const onboardingCompleted = backendOk
        ? onboardingCompletedInDb
        : isOnboardingCompletedLocal();
      if (!onboardingCompleted) {
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

  const handleOnboardingDone = useCallback(() => {
    markOnboardingCompletedLocal();
    settingsSet(ONBOARDING_KEY, '1').catch(() => {});
    setShowOnboarding(false);
  }, []);

  return (
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
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
      // show the main window AND close the always-on-top splash. The splash
      // is normally closed by the backend (try_finish_startup) once both
      // sides are ready — on a render crash that never happens, so the
      // error UI would otherwise stay covered by the splash forever.
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
// ball, pet-chat windows the popped-out conversation, note windows just the
// opened note, everything else is the app.
let windowKind: 'app' | 'pet' | 'pet-chat' | 'note' = 'app';
try {
  const label = getCurrentWindow().label;
  if (label === 'pet') windowKind = 'pet';
  else if (label.startsWith('pet-chat-')) windowKind = 'pet-chat';
  else if (label.startsWith('note-')) windowKind = 'note';
} catch {
  windowKind = 'app';
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  windowKind === 'pet' ? (
    <PetBallWindow />
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
