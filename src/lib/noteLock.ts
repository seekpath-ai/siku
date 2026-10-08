import type { Note } from './types';

/** Effective lock set: notes with is_locked = 1 plus ALL their descendants
 *  (a locked folder locks its whole subtree). The tree is in memory, so this
 *  is a parent_id map + DFS. */
export function computeEffectiveLockedSet(notes: Note[]): Set<string> {
  const childrenOf = new Map<string, string[]>();
  for (const n of notes) {
    const pid = n.parent_id ?? '';
    const list = childrenOf.get(pid);
    if (list) list.push(n.id);
    else childrenOf.set(pid, [n.id]);
  }
  const locked = new Set<string>();
  const stack = notes.filter((n) => n.is_locked === 1).map((n) => n.id);
  while (stack.length > 0) {
    const id = stack.pop()!;
    if (locked.has(id)) continue;
    locked.add(id);
    for (const c of childrenOf.get(id) ?? []) stack.push(c);
  }
  return locked;
}

export function isEffectivelyLocked(set: Set<string>, id: string): boolean {
  return set.has(id);
}

/** A note is hidden (cover / filtered) when it is effectively locked and its
 *  id has not been unlocked this session. */
export function isLockHidden(lockedSet: Set<string>, unlockedSet: Set<string>, id: string): boolean {
  return lockedSet.has(id) && !unlockedSet.has(id);
}
