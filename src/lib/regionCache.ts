// ── Persistent region cache (localStorage-backed) ──
// Survives tab close and page refresh. Keyed by paperId + pageIndex.

import type { DetectedRegion } from '@/components/reader/regions';

const STORAGE_KEY_PREFIX = 'siku_regions_';
const MAX_ENTRIES = 500; // max total cached pages across all papers

interface CacheEntry {
  paperId: string;
  pageIndex: number;
  regions: DetectedRegion[];
  /** How the regions were obtained: 'rule' | 'llm' | 'import' */
  source: 'rule' | 'llm' | 'import';
  cachedAt: string; // ISO timestamp
}

interface CacheManifest {
  version: 1;
  entries: CacheEntry[];
}

function loadManifest(): CacheManifest {
  try {
    const raw = localStorage.getItem(STORAGE_KEY_PREFIX + 'manifest');
    if (!raw) return { version: 1, entries: [] };
    const parsed = JSON.parse(raw);
    if (parsed.version === 1 && Array.isArray(parsed.entries)) return parsed;
  } catch { /* corrupted, start fresh */ }
  return { version: 1, entries: [] };
}

function saveManifest(m: CacheManifest): void {
  try {
    localStorage.setItem(STORAGE_KEY_PREFIX + 'manifest', JSON.stringify(m));
  } catch { /* storage full, silently drop */ }
}

function evictOldest(m: CacheManifest): void {
  while (m.entries.length > MAX_ENTRIES) {
    m.entries.shift(); // oldest first
  }
}

/** Load cached regions for a specific paper+page. Returns null if not found. */
export function loadCachedRegions(
  paperId: string,
  pageIndex: number,
): { regions: DetectedRegion[]; source: CacheEntry['source'] } | null {
  const m = loadManifest();
  const entry = m.entries.find(
    (e) => e.paperId === paperId && e.pageIndex === pageIndex,
  );
  if (!entry) return null;
  return { regions: entry.regions, source: entry.source };
}

/** Load all cached regions for a paper. Returns empty array if none. */
export function loadPaperCache(paperId: string): DetectedRegion[] {
  const m = loadManifest();
  return m.entries
    .filter((e) => e.paperId === paperId)
    .flatMap((e) => e.regions);
}

/** Save regions for a paper+page. Overwrites existing entry for same page. */
export function saveCachedRegions(
  paperId: string,
  pageIndex: number,
  regions: DetectedRegion[],
  source: CacheEntry['source'],
): void {
  if (regions.length === 0) return;
  const m = loadManifest();
  // Remove existing entry for same paper+page
  const idx = m.entries.findIndex(
    (e) => e.paperId === paperId && e.pageIndex === pageIndex,
  );
  if (idx >= 0) m.entries.splice(idx, 1);
  m.entries.push({
    paperId,
    pageIndex,
    regions,
    source,
    cachedAt: new Date().toISOString(),
  });
  evictOldest(m);
  saveManifest(m);
}

/** Clear all cached regions for a specific paper. */
export function clearPaperCache(paperId: string): void {
  const m = loadManifest();
  m.entries = m.entries.filter((e) => e.paperId !== paperId);
  saveManifest(m);
}

/** Clear all cached regions across all papers. */
export function clearAllCache(): void {
  localStorage.removeItem(STORAGE_KEY_PREFIX + 'manifest');
}
