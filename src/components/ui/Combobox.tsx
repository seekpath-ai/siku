import { useState, useRef, useEffect, useMemo } from 'react';
import { createPortal } from 'react-dom';
import { ChevronDown, Check } from 'lucide-react';

export interface ComboboxOption {
  value: string;
  label?: string;
}

interface Props {
  value: string;
  onChange: (value: string) => void;
  options: (string | ComboboxOption)[];
  placeholder?: string;
  id?: string;
}

/** Max height of the option list, px (matches max-h-60). */
const LIST_MAX_HEIGHT = 240;

interface ListPos {
  left: number;
  width: number;
  top: number;
  bottom: number;
  /** Open upward instead of downward when there is not enough space below. */
  openAbove: boolean;
}

/**
 * Editable combobox (input + listbox), mainstream behavior:
 * - Clicking the field / chevron opens the list showing ALL options.
 * - Typing filters the list; clearing the input shows all options again.
 * - The text is select-all on focus so typing immediately replaces it.
 * - Arrow keys navigate, Enter selects, Escape reverts, Tab commits.
 * - The list is rendered in a portal with fixed positioning, so it never
 *   creates a second page scrollbar and flips upward when space is tight.
 */
export function Combobox({ value, onChange, options, placeholder, id }: Props) {
  const [open, setOpen] = useState(false);
  const [inputValue, setInputValue] = useState(value);
  const [highlightedIndex, setHighlightedIndex] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const [listPos, setListPos] = useState<ListPos | null>(null);

  const normalized = useMemo(
    () =>
      options.map((opt) =>
        typeof opt === 'string' ? { value: opt, label: opt } : opt,
      ),
    [options],
  );

  // The label of the currently selected option (falls back to the raw value
  // when nothing matches — e.g. a stale id after an option was deleted).
  const selectedLabel = useMemo(
    () => normalized.find((o) => o.value === value)?.label ?? value,
    [normalized, value],
  );

  // Resolve typed/committed text back to an option value (label or value).
  const resolveValue = (text: string): string => {
    const t = text.trim();
    const hit = normalized.find((o) => o.label === t || o.value === t);
    return hit ? hit.value : t;
  };

  // Filter only when the user has actually edited the text (input differs
  // from the selected label). Opening with the selection untouched shows
  // every option instead of narrowing to the current value.
  const activeQuery = useMemo(() => {
    const q = inputValue.trim().toLowerCase();
    if (!q || q === selectedLabel.trim().toLowerCase()) return '';
    return q;
  }, [inputValue, selectedLabel]);

  const filtered = useMemo(() => {
    if (!activeQuery) return normalized;
    return normalized.filter(
      (opt) =>
        opt.value.toLowerCase().includes(activeQuery) ||
        (opt.label && opt.label.toLowerCase().includes(activeQuery)),
    );
  }, [normalized, activeQuery]);

  useEffect(() => {
    setInputValue(selectedLabel);
  }, [selectedLabel]);

  useEffect(() => {
    setHighlightedIndex(0);
  }, [inputValue, open]);

  // Position the portaled list against the input and keep it aligned while
  // the page scrolls or the window resizes.
  useEffect(() => {
    if (!open) return;
    const update = () => {
      const input = inputRef.current;
      if (!input) return;
      const r = input.getBoundingClientRect();
      const spaceBelow = window.innerHeight - r.bottom;
      const openAbove = spaceBelow < LIST_MAX_HEIGHT && r.top > spaceBelow;
      setListPos({ left: r.left, width: r.width, top: r.top, bottom: r.bottom, openAbove });
    };
    update();
    window.addEventListener('scroll', update, true);
    window.addEventListener('resize', update);
    return () => {
      window.removeEventListener('scroll', update, true);
      window.removeEventListener('resize', update);
    };
  }, [open]);

  // Close when clicking outside the input and the (portaled) list.
  useEffect(() => {
    const close = (e: MouseEvent) => {
      const t = e.target as Node;
      if (containerRef.current?.contains(t) || listRef.current?.contains(t)) return;
      setOpen(false);
    };
    if (open) document.addEventListener('mousedown', close, true);
    return () => document.removeEventListener('mousedown', close, true);
  }, [open]);

  const selectOption = (opt: ComboboxOption) => {
    onChange(opt.value);
    setInputValue(opt.label || opt.value);
    setOpen(false);
    inputRef.current?.blur();
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (!open && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      setOpen(true);
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setHighlightedIndex((i) => Math.min(i + 1, Math.max(filtered.length - 1, 0)));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setHighlightedIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (open && filtered[highlightedIndex]) {
        selectOption(filtered[highlightedIndex]);
      } else {
        onChange(resolveValue(inputValue));
        setInputValue(selectedLabel);
        setOpen(false);
        inputRef.current?.blur();
      }
    } else if (e.key === 'Escape') {
      setOpen(false);
      setInputValue(selectedLabel);
    } else if (e.key === 'Tab') {
      onChange(resolveValue(inputValue));
      setInputValue(selectedLabel);
      setOpen(false);
    }
  };

  const list = open && listPos
    ? createPortal(
        <ul
          ref={listRef}
          role="listbox"
          className="fixed z-50 overflow-y-auto rounded-lg border border-surface-hover bg-surface shadow-xl py-1"
          style={{
            left: listPos.left,
            width: listPos.width,
            top: listPos.openAbove ? undefined : listPos.bottom + 4,
            bottom: listPos.openAbove ? window.innerHeight - listPos.top + 4 : undefined,
            maxHeight: listPos.openAbove
              ? Math.min(LIST_MAX_HEIGHT, listPos.top - 8)
              : Math.min(LIST_MAX_HEIGHT, window.innerHeight - listPos.bottom - 8),
          }}
        >
          {filtered.length === 0 ? (
            <li className="px-3 py-2 text-sm text-text-secondary/50 select-none">
              无匹配选项
            </li>
          ) : (
            filtered.map((opt, idx) => {
              const selected = opt.value === value;
              const highlighted = idx === highlightedIndex;
              return (
                <li
                  key={opt.value}
                  role="option"
                  aria-selected={selected}
                  onMouseEnter={() => setHighlightedIndex(idx)}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    selectOption(opt);
                  }}
                  className={`flex items-center justify-between px-3 py-2 text-sm cursor-pointer ${
                    highlighted || selected
                      ? 'bg-primary/10 text-text-primary'
                      : 'text-text-secondary'
                  }`}
                >
                  <span>{opt.label || opt.value}</span>
                  {selected && <Check size={14} className="text-primary" />}
                </li>
              );
            })
          )}
        </ul>,
        document.body,
      )
    : null;

  return (
    <div ref={containerRef} className="relative">
      <div className="flex items-center relative">
        <input
          ref={inputRef}
          id={id}
          value={inputValue}
          onChange={(e) => {
            setInputValue(e.target.value);
            setOpen(true);
          }}
          onFocus={(e) => {
            setOpen(true);
            e.target.select();
          }}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          className="w-full bg-surface border border-surface-hover rounded-lg pl-3 pr-9 py-2 text-sm text-text-primary focus:outline-none focus:border-primary"
        />
        <button
          type="button"
          tabIndex={-1}
          onClick={() => {
            setOpen((v) => !v);
            inputRef.current?.focus();
          }}
          className="absolute right-2 p-0.5 text-text-secondary hover:text-text-primary"
          aria-label="展开选项"
        >
          <ChevronDown
            size={16}
            className={`transition-transform ${open ? 'rotate-180' : ''}`}
          />
        </button>
      </div>

      {list}
    </div>
  );
}
