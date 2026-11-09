import { HelpCircle } from 'lucide-react';

/** Small "?" icon with a hover tooltip panel for setup guidance.
 *  The gap between icon and panel is bridged with padding (not margin) so
 *  the mouse can travel into the panel without losing hover — required for
 *  selecting/copying text like event names. */
export function HintIcon({ children }: { children: React.ReactNode }) {
  return (
    <span className="relative group inline-flex items-center align-middle ml-1">
      <HelpCircle size={14} className="text-text-secondary/40 group-hover:text-primary cursor-help transition-colors" />
      <span className="hidden group-hover:block absolute left-0 top-full pt-1.5 z-50 w-80 max-w-[80vw] select-text cursor-default">
        <span className="block rounded-lg border border-surface-hover bg-surface shadow-xl px-3 py-2.5 text-xs leading-relaxed text-text-secondary font-normal">
          {children}
        </span>
      </span>
    </span>
  );
}
