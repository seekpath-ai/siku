import type { Note } from './types';

/** Hidden (gated) set: the union of subtrees rooted at every LOCK ROOT — a
 *  note with is_locked = 1 that has NOT been session-unlocked. Unlocking a
 *  folder clears its whole subtree from this set; a descendant with its own
 *  is_locked = 1 stays a lock root on its own and keeps its subtree hidden. */
export function computeHiddenSet(notes: Note[], unlockedSet: Set<string>): Set<string> {
  const childrenOf = new Map<string, string[]>();
  for (const n of notes) {
    const pid = n.parent_id ?? '';
    const list = childrenOf.get(pid);
    if (list) list.push(n.id);
    else childrenOf.set(pid, [n.id]);
  }
  const hidden = new Set<string>();
  const stack = notes
    .filter((n) => n.is_locked === 1 && !unlockedSet.has(n.id))
    .map((n) => n.id);
  while (stack.length > 0) {
    const id = stack.pop()!;
    if (hidden.has(id)) continue;
    hidden.add(id);
    for (const c of childrenOf.get(id) ?? []) stack.push(c);
  }
  return hidden;
}

/** Lock-root ids gating a note: itself plus every ancestor with is_locked = 1
 *  that is not session-unlocked yet. Verifying the password once unlocks all
 *  of them (unlockMany), so opening a note inside a locked folder releases
 *  the whole folder subtree. Empty when the note is not gated. */
export function computeGateIds(notes: Note[], noteId: string, unlockedSet: Set<string>): string[] {
  const byId = new Map(notes.map((n) => [n.id, n]));
  const ids: string[] = [];
  let cur: string | null = noteId;
  let hops = 0;
  while (cur && hops < 100) {
    const n = byId.get(cur);
    if (!n) break;
    if (n.is_locked === 1 && !unlockedSet.has(cur)) ids.push(cur);
    cur = n.parent_id;
    hops += 1;
  }
  return ids;
}
