import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { ChevronRight } from "lucide-react";

export interface ContextMenuItem {
  label: string;
  icon?: ReactNode;
  destructive?: boolean;
  disabled?: boolean;
  separatorBefore?: boolean;
  /** Parent items with children open a submenu on hover instead of firing. */
  children?: ContextMenuItem[];
  onClick?: () => void;
}

interface ContextMenuProps {
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
  maxHeight?: number;
}

const VIEWPORT_MARGIN = 8;
const SUBMENU_WIDTH = 180;

function MenuItem({ item, onClose }: { item: ContextMenuItem; onClose: () => void }) {
  const [subPos, setSubPos] = useState<{ left: number; top: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);

  if (!item.children) {
    return (
      <button
        disabled={item.disabled}
        onClick={(e) => {
          e.stopPropagation();
          item.onClick?.();
          onClose();
        }}
        className={`flex w-full items-center gap-2 px-3 py-1.5 text-xs transition-colors disabled:opacity-30 ${
          item.destructive
            ? "text-red-400 hover:bg-red-500/10"
            : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
        }`}
      >
        {item.icon && <span className="shrink-0">{item.icon}</span>}
        <span>{item.label}</span>
      </button>
    );
  }

  // The submenu is position:fixed, NOT absolute inside the menu: the root
  // menu is a scroll container (overflow-y-auto), which would clip an
  // absolutely-positioned child and grow a horizontal scrollbar.
  return (
    <div
      onMouseEnter={() => {
        if (item.disabled) return;
        const rect = btnRef.current?.getBoundingClientRect();
        if (!rect) return;
        const flip = rect.right + SUBMENU_WIDTH > window.innerWidth - VIEWPORT_MARGIN;
        setSubPos({
          left: flip ? rect.left - SUBMENU_WIDTH : rect.right,
          top: Math.min(rect.top, window.innerHeight - VIEWPORT_MARGIN - item.children!.length * 32),
        });
      }}
      onMouseLeave={() => setSubPos(null)}
    >
      <button
        ref={btnRef}
        disabled={item.disabled}
        className="flex w-full items-center gap-2 px-3 py-1.5 text-xs text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary disabled:opacity-30"
      >
        {item.icon && <span className="shrink-0">{item.icon}</span>}
        <span>{item.label}</span>
        <ChevronRight size={12} className="ml-auto shrink-0 opacity-60" />
      </button>
      {subPos && !item.disabled && (
        <div
          className="fixed z-10 min-w-[140px] overflow-y-auto rounded-lg border border-surface-hover bg-surface py-1 shadow-xl"
          style={{
            left: subPos.left,
            top: subPos.top,
            width: SUBMENU_WIDTH,
            maxHeight: "calc(100dvh - 16px)",
          }}
        >
          {item.children.map((child, i) => (
            <Fragment key={i}>
              {child.separatorBefore && (
                <div className="mx-2 my-1 border-t border-surface-hover" />
              )}
              <MenuItem item={child} onClose={onClose} />
            </Fragment>
          ))}
        </div>
      )}
    </div>
  );
}

export function ContextMenu({ x, y, items, onClose, maxHeight }: ContextMenuProps) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });

  // Close on click-away
  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        onClose();
      }
    };
    document.addEventListener("click", onClick, true);
    return () => document.removeEventListener("click", onClick, true);
  }, [onClose]);

  // Close on Escape
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  // Measure the menu after it is actually mounted and clamp it inside the
  // viewport. (Checking the ref during render never worked — the portal child
  // is only attached after the render — so menus near the bottom/right edge
  // overflowed and got clipped by the window.)
  useLayoutEffect(() => {
    const updatePosition = () => {
      const el = menuRef.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      const nx = Math.max(
        VIEWPORT_MARGIN,
        Math.min(x, window.innerWidth - r.width - VIEWPORT_MARGIN),
      );
      const ny = Math.max(
        VIEWPORT_MARGIN,
        Math.min(y, window.innerHeight - r.height - VIEWPORT_MARGIN),
      );
      setPos((prev) =>
        prev.x === nx && prev.y === ny ? prev : { x: nx, y: ny },
      );
    };
    updatePosition();
    window.addEventListener("resize", updatePosition);
    return () => window.removeEventListener("resize", updatePosition);
  }, [x, y, items]);

  return createPortal(
    <div
      ref={menuRef}
      className="fixed z-50 min-w-[160px] overflow-y-auto rounded-lg border border-surface-hover bg-surface py-1 shadow-xl"
      style={{
        left: pos.x,
        top: pos.y,
        maxWidth: `calc(100dvw - ${VIEWPORT_MARGIN * 2}px)`,
        maxHeight:
          maxHeight === undefined
            ? `calc(100dvh - ${VIEWPORT_MARGIN * 2}px)`
            : `min(${maxHeight}px, calc(100dvh - ${VIEWPORT_MARGIN * 2}px))`,
      }}
      onContextMenu={(e) => e.preventDefault()}
      // Keep focus where it was (editor / table cell input) while the menu is
      // open: a focus steal would blur an open cell editor and race its
      // blur-commit ahead of the menu action. Clicks on the menu's own
      // scrollbar (target === container) must keep their default behaviour.
      onMouseDown={(e) => {
        if (e.target !== e.currentTarget) e.preventDefault();
      }}
    >
      {items.map((item, i) => (
        <Fragment key={i}>
          {item.separatorBefore && (
            <div className="mx-2 my-1 border-t border-surface-hover" />
          )}
          <MenuItem item={item} onClose={onClose} />
        </Fragment>
      ))}
    </div>,
    document.body,
  );
}
