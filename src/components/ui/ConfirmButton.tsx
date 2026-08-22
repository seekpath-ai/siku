import { useState, useEffect, useCallback, useRef, type MouseEvent, type ReactNode } from 'react';

interface ConfirmButtonProps {
  /** Called on the second click (when confirming) */
  onConfirm: () => void;
  /** Button content — pass icon or text */
  children: ReactNode;
  /** Text shown during confirmation phase (default: "确认?") */
  confirmText?: string;
  /** Extra class names */
  className?: string;
  /** Whether to show as an icon button (compact, just the icon) */
  icon?: boolean;
  /** aria-label for accessibility */
  'aria-label'?: string;
}

/**
 * Two-click confirmation button for destructive actions.
 * First click enters "confirming" state, second click executes.
 * Auto-cancels after 3s or on click-away.
 */
export function ConfirmButton({
  onConfirm,
  children,
  confirmText,
  className = '',
  icon = false,
  'aria-label': ariaLabel,
}: ConfirmButtonProps) {
  const [confirming, setConfirming] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);
  const wrapperRef = useRef<HTMLElement>(null);

  const handleClick = useCallback(
    (e: MouseEvent<HTMLButtonElement>) => {
      e.stopPropagation();
      if (!confirming) {
        setConfirming(true);
        return;
      }
      setConfirming(false);
      onConfirm();
    },
    [confirming, onConfirm],
  );

  // Auto-cancel after 3s
  useEffect(() => {
    if (!confirming) return;
    const tid = setTimeout(() => setConfirming(false), 3000);
    return () => clearTimeout(tid);
  }, [confirming]);

  // Cancel on click-away (but not inside this component)
  useEffect(() => {
    if (!confirming) return;
    const onClick = (e: Event) => {
      const el = wrapperRef.current || btnRef.current;
      if (el && !el.contains(e.target as Node)) {
        setConfirming(false);
      }
    };
    // Use capture phase so we see the event regardless of stopPropagation
    document.addEventListener('click', onClick, true);
    return () => document.removeEventListener('click', onClick, true);
  }, [confirming]);

  const label = confirmText || '确认?';

  if (icon) {
    return (
      <span
        ref={wrapperRef as React.RefObject<HTMLSpanElement>}
        onClick={handleClick}
        className={`inline-flex items-center gap-1 cursor-pointer ${className}`}
      >
        <button
          ref={btnRef}
          onClick={handleClick}
          aria-label={confirming ? label : ariaLabel}
          className={`shrink-0 p-0.5 rounded transition-colors ${
            confirming
              ? 'text-red-400 bg-red-500/10'
              : 'text-text-secondary/30 hover:text-red-400 hover:bg-red-500/10'
          }`}
        >
          {children}
        </button>
        {confirming && (
          <span className="text-[10px] text-red-400 select-none whitespace-nowrap">{label}</span>
        )}
      </span>
    );
  }

  return (
    <button
      ref={(el) => {
        (btnRef as React.MutableRefObject<HTMLButtonElement | null>).current = el;
        (wrapperRef as React.MutableRefObject<HTMLButtonElement | null>).current = el;
      }}
      onClick={handleClick}
      className={`text-[10px] transition-colors ${
        confirming
          ? 'text-red-400'
          : 'text-text-secondary/40 hover:text-red-400'
      } ${className}`}
    >
      {confirming ? label : children}
    </button>
  );
}
