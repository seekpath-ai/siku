/** Line-level diff (LCS-based) for the note version comparison view. */

export type DiffKind = 'same' | 'del' | 'add';

export interface DiffLine {
  kind: DiffKind;
  text: string;
}

export interface DiffResult {
  /** Lines of the base (current) side, aligned with `right`. */
  left: (DiffLine | null)[];
  /** Lines of the target (selected) side, aligned with `left`. */
  right: (DiffLine | null)[];
  added: number;
  removed: number;
}

function lcsLengths(a: string[], b: string[]): number[][] {
  const n = a.length;
  const m = b.length;
  // dp[i][j] = LCS length of a[i..] and b[j..]
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  return dp;
}

/** Diff two texts line by line. */
export function diffLines(base: string, target: string): DiffResult {
  const a = base.split('\n');
  const b = target.split('\n');
  const dp = lcsLengths(a, b);

  const left: (DiffLine | null)[] = [];
  const right: (DiffLine | null)[] = [];
  let added = 0;
  let removed = 0;

  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      left.push({ kind: 'same', text: a[i] });
      right.push({ kind: 'same', text: b[j] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      left.push({ kind: 'del', text: a[i] });
      right.push(null);
      removed++;
      i++;
    } else {
      left.push(null);
      right.push({ kind: 'add', text: b[j] });
      added++;
      j++;
    }
  }
  while (i < a.length) {
    left.push({ kind: 'del', text: a[i] });
    right.push(null);
    removed++;
    i++;
  }
  while (j < b.length) {
    left.push(null);
    right.push({ kind: 'add', text: b[j] });
    added++;
    j++;
  }

  return { left, right, added, removed };
}
