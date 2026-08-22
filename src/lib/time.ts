import { format, parseISO } from 'date-fns';

/**
 * Convert ISO 8601 UTC string to display format.
 * "2026-06-16T10:00:00Z" → "2026-06-16 10:00"
 */
export function isoToDisplay(iso: string): string {
  try {
    return format(parseISO(iso), 'yyyy-MM-dd HH:mm');
  } catch {
    return iso;
  }
}

/**
 * Convert ISO 8601 UTC string to date-only format.
 * "2026-06-16T10:00:00Z" → "2026-06-16"
 */
export function isoToDate(iso: string): string {
  try {
    return format(parseISO(iso), 'yyyy-MM-dd');
  } catch {
    return iso;
  }
}

/**
 * Convert ISO 8601 UTC string to time-only format.
 * "2026-06-16T10:00:00Z" → "10:00"
 */
export function isoToTime(iso: string): string {
  try {
    return format(parseISO(iso), 'HH:mm');
  } catch {
    return iso;
  }
}
