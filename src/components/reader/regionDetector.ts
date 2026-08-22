import type { TextItemWithPosition, DetectedRegion, PageRegionResult } from './regions';

// ── Math font name substrings ──
const MATH_FONTS = ['CMSY', 'CMMI', 'MSAM', 'MSBM', 'CMEX', 'CMR', 'Symbol', 'Math'];
const FIGURE_CAPTION_RE = /^(?:Fig(?:ure)?\.?\s*\d+|Fig\.\s*\d+)/i;
const TABLE_CAPTION_RE = /^Table\.?\s*\d+/i;
const ABSTRACT_HEADING_RE = /^(?:Abstract|ABSTRACT|摘要)$/;
const REFS_HEADING_RE = /^(?:References|REFERENCES|Bibliography|Works Cited|参考文献)$/;
const HEADING_NUM_RE = /^(?:\d+\.?\s+|[IVX]+\.\s+)[A-Z].*$/;

/** Compute median of sorted numbers. */
function median(sorted: number[]): number {
  if (sorted.length === 0) return 0;
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

// ── Internal line / block types ──
interface Line {
  items: TextItemWithPosition[];
  y: number;          // mean Y
  maxFontSize: number;
  text: string;       // joined text
  xMin: number;
  xMax: number;
}

interface Block {
  lines: Line[];
  yTop: number;
  yBottom: number;
  xMin: number;
  xMax: number;
  maxFontSize: number;
  meanFontSize: number;
  text: string;
  isCentered: boolean;
  lineCount: number;
}

// ────────────────────────────────────────────────
// 1. Convert & normalize items
// ────────────────────────────────────────────────

/** Convert raw pdf.js text items to position-aware items.
 *  pdf.js transform: [a,b,c,d,e,f] where e=X offset, f=Y offset (from page bottom).
 *  We flip Y to be from page top for easier top-down analysis. */
function normalizeItems(
  rawItems: { str: string; transform: number[]; height?: number; fontName?: string; width?: number }[],
  pageHeight: number,
): TextItemWithPosition[] {
  const items: TextItemWithPosition[] = [];
  for (const item of rawItems) {
    const s = item.str;
    if (!s || !s.trim()) continue;
    const t = item.transform;
    const fontSize = item.height ?? Math.abs(t[3]) ?? Math.abs(t[0]) ?? 12;
    // Estimate width from char count × font size when pdf.js doesn't provide it
    const estWidth = s.length * fontSize * 0.55;
    const width = (item.width && item.width > 0) ? item.width : estWidth;
    items.push({
      str: s,
      x: t[4],
      y: pageHeight - t[5], // flip: PDF bottom-up → top-down
      fontSize,
      fontName: item.fontName ?? '',
      width,
    });
  }
  return items;
}

// ────────────────────────────────────────────────
// 2. Cluster items into lines
// ────────────────────────────────────────────────

function clusterLines(items: TextItemWithPosition[], pageWidth: number): Line[] {
  if (items.length === 0) return [];

  const sorted = [...items].sort((a, b) => a.y - b.y); // top→bottom
  const Y_TOL = 2;
  const COLUMN_GAP = pageWidth * 0.10; // X-gap > 10% page width = column boundary

  const rawLines: TextItemWithPosition[][] = [];
  for (const item of sorted) {
    let placed = false;
    for (const line of rawLines) {
      if (Math.abs(line[0].y - item.y) <= Y_TOL) {
        line.push(item);
        placed = true;
        break;
      }
    }
    if (!placed) rawLines.push([item]);
  }

  // Sort within each line left→right, then split by X-gap for multi-column
  const lines: Line[] = [];
  for (const raw of rawLines) {
    raw.sort((a, b) => a.x - b.x);

    // Split raw line into column groups by large X-gaps
    const columns: TextItemWithPosition[][] = [];
    let colStart = 0;
    for (let i = 1; i < raw.length; i++) {
      const prevRight = raw[i - 1].x + raw[i - 1].width;
      const gap = raw[i].x - prevRight;
      if (gap > COLUMN_GAP) {
        columns.push(raw.slice(colStart, i));
        colStart = i;
      }
    }
    columns.push(raw.slice(colStart));

    // Each column group becomes a separate line
    for (const colItems of columns) {
      const text = colItems.map(it => it.str).join(' ').trim();
      if (!text) continue;
      const xMin = Math.min(...colItems.map(it => it.x));
      const xMax = Math.max(...colItems.map(it => it.x + it.width));
      // Diagnose: log the rightmost item that determines xMax
      const rightmost = colItems.reduce((a, b) => (a.x + a.width) > (b.x + b.width) ? a : b);
      console.debug(
        '[clusterLines] line y≈' + Math.round(colItems.reduce((s, it) => s + it.y, 0) / colItems.length) +
        ' items=' + colItems.length +
        ' xMin=' + Math.round(xMin) +
        ' xMax=' + Math.round(xMax) +
        ' rightmost="' + rightmost.str + '" x=' + Math.round(rightmost.x) + ' w=' + Math.round(rightmost.width) +
        ' x+w=' + Math.round(rightmost.x + rightmost.width),
      );
      lines.push({
        items: colItems,
        y: colItems.reduce((s, it) => s + it.y, 0) / colItems.length,
        maxFontSize: Math.max(...colItems.map(it => it.fontSize)),
        text,
        xMin,
        xMax,
      });
    }
  }

  // Sort top→bottom, then left→right (same-Y columns L→R)
  lines.sort((a, b) => a.y - b.y || a.xMin - b.xMin);

  return lines;
}

// ────────────────────────────────────────────────
// 3. Cluster lines into blocks
// ────────────────────────────────────────────────

function clusterBlocks(lines: Line[], pageWidth: number): Block[] {
  if (lines.length === 0) return [];
  if (lines.length === 1) return [buildBlock([lines[0]])];

  const COLUMN_X_GAP = pageWidth * 0.12; // xMin diff > 12% page width = different column

  // Compute raw Y gaps between consecutive lines
  const rawGaps: number[] = [];
  for (let i = 1; i < lines.length; i++) {
    rawGaps.push(lines[i].y - lines[i - 1].y);
  }

  // Normal line spacing = median of all raw gaps
  const sortedGaps = [...rawGaps].sort((a, b) => a - b);
  const normalSpacing = median(sortedGaps);

  // Also compute median line height for font-size-change detection
  const allHeights = lines.map(l => l.maxFontSize).sort((a, b) => a - b);
  const medianH = median(allHeights);

  // Break threshold: 20% larger than normal spacing catches paragraph gaps
  // while staying safely above intra-paragraph line spacing (~1.0× normal).
  const breakThreshold = Math.max(normalSpacing * 1.2, medianH * 0.2, 4);

  // Group lines into blocks
  const blocks: Block[] = [];
  let blockLines: Line[] = [lines[0]];

  for (let i = 1; i < lines.length; i++) {
    const prev = lines[i - 1];
    const curr = lines[i];
    const rawGap = curr.y - prev.y;
    const prevFont = prev.maxFontSize;
    const currFont = curr.maxFontSize;
    const fontJump = Math.abs(currFont - prevFont) > medianH * 0.3;

    // Same Y but very different xMin → different column, force break
    const crossColumn = rawGap < normalSpacing * 0.3 && Math.abs(curr.xMin - prev.xMin) > COLUMN_X_GAP;

    // A significant gap OR a font-size jump OR a column switch → new block
    const isBreak = crossColumn || rawGap > breakThreshold || (rawGap > normalSpacing * 1.1 && fontJump);

    if (isBreak) {
      blocks.push(buildBlock(blockLines));
      blockLines = [curr];
    } else {
      blockLines.push(curr);
    }
  }
  blocks.push(buildBlock(blockLines));
  return blocks;
}

function buildBlock(lines: Line[]): Block {
  const text = lines.map(l => l.text).join(' ');
  const allFontSizes: number[] = [];
  for (const l of lines) {
    for (const it of l.items) allFontSizes.push(it.fontSize);
  }
  const meanFS = allFontSizes.reduce((s, v) => s + v, 0) / allFontSizes.length;

  // In flipped (top-down) coordinates:
  // - Text baseline is at item.y
  // - Visual top ≈ baseline - fontSize (full cap-height + ascender margin)
  // - Visual bottom ≈ baseline (descenders are small, ~0.2×fontSize but negligible)
  // Use per-item min/max for robustness instead of line-mean estimates.
  const firstFont = lines[0].maxFontSize;
  const firstLineItems = lines[0].items;
  const yTop = Math.min(...firstLineItems.map(it => it.y)) - firstFont;
  const lastLineItems = lines[lines.length - 1].items;
  const yBottom = Math.max(...lastLineItems.map(it => it.y));

  const blockXMin = Math.min(...lines.map(l => l.xMin));
  const blockXMax = Math.max(...lines.map(l => l.xMax));

  // Diagnose: log line x-ranges contributing to block bounds
  if (lines.length > 1) {
    const lineRanges = lines.map(l =>
      `y≈${Math.round(l.y)} x${Math.round(l.xMin)}-${Math.round(l.xMax)}`
    ).join(', ');
    console.debug(
      '[buildBlock] lines=' + lines.length +
      ' blockX=' + Math.round(blockXMin) + '-' + Math.round(blockXMax) +
      ' blockW=' + Math.round(blockXMax - blockXMin) +
      ' | ' + lineRanges,
    );
  }

  return {
    lines,
    yTop,
    yBottom,
    xMin: blockXMin,
    xMax: blockXMax,
    maxFontSize: Math.max(...lines.map(l => l.maxFontSize)),
    meanFontSize: meanFS,
    text,
    isCentered: false,
    lineCount: lines.length,
  };
}

// ────────────────────────────────────────────────
// 4. Classification rules
// ────────────────────────────────────────────────

interface ClassifyContext {
  pageIndex: number;
  pageWidth: number;
  pageHeight: number;
  totalPages: number;
  bodyFontSize: number; // most common font size on this page
}

/** Clamp a number to [0, 1]. */
function clamp01(v: number): number {
  return Math.max(0, Math.min(1, v));
}

/** Gaussian-like score: 1 at target, decaying as value moves away. */
function proximityScore(value: number, target: number, tolerance: number): number {
  if (tolerance <= 0) return value === target ? 1 : 0;
  return clamp01(1 - Math.abs(value - target) / tolerance);
}

// ── Real confidence scoring helpers ──
// These replace the previous hard-coded per-type confidence constants.
// Each function returns [0, 1] based on how strongly the block matches the
// expected textual, typographic, and positional features of that region type.

function scoreTitle(block: Block, ctx: ClassifyContext, fontSizeRatio: number): number {
  const inTop = block.yTop < ctx.pageHeight * 0.3 ? 1 : 0.4;
  const sizeScore = clamp01((fontSizeRatio - 1.2) / 0.8); // 1.2→0, 2.0→1
  const centered = block.isCentered ? 1 : 0.7;
  const len = block.text.trim().length;
  const lengthScore = len >= 20 && len <= 200 ? 1 : len > 200 ? 0.7 : 0.4;
  return clamp01(0.55 + sizeScore * 0.25 + inTop * 0.1 + centered * 0.05 + lengthScore * 0.05);
}

function scoreAuthors(block: Block, fontSizeRatio: number): number {
  const text = block.text.trim();
  const separatorCount = (text.match(/[,;†*‡§¶]/g) || []).length;
  const hasEmail = /@/.test(text);
  const hasAffiliation = /\b(?:University|Institute|College|Lab|School|Dept|Department|Center)\b/i.test(text);
  const patternScore = clamp01((separatorCount * 0.15) + (hasEmail ? 0.25 : 0) + (hasAffiliation ? 0.15 : 0));
  const sizeScore = proximityScore(fontSizeRatio, 1.25, 0.3);
  const lineScore = block.lineCount <= 3 ? 1 : block.lineCount <= 5 ? 0.7 : 0.4;
  return clamp01(0.45 + patternScore * 0.35 + sizeScore * 0.1 + lineScore * 0.1);
}

function scoreAbstractHeading(block: Block): number {
  const text = block.text.trim();
  if (!ABSTRACT_HEADING_RE.test(text.split(/\s+/)[0])) return 0.3;
  const lenScore = text.length <= 10 ? 1 : 0.6;
  const alone = block.lineCount === 1 ? 1 : 0.7;
  return clamp01(0.6 + lenScore * 0.2 + alone * 0.2);
}

function scoreAbstractBody(block: Block, fontSizeRatio: number): number {
  const sizeScore = proximityScore(fontSizeRatio, 1.0, 0.15);
  const lineScore = clamp01(block.lineCount / 4); // more lines = more confident
  return clamp01(0.45 + sizeScore * 0.25 + lineScore * 0.3);
}

function scoreReferencesHeading(block: Block): number {
  const text = block.text.trim();
  if (!REFS_HEADING_RE.test(text.split(/\s+/)[0])) return 0.3;
  const lenScore = text.length <= 12 ? 1 : 0.7;
  const alone = block.lineCount === 1 ? 1 : 0.8;
  return clamp01(0.6 + lenScore * 0.2 + alone * 0.2);
}

function scoreReferencesBody(block: Block, fontSizeRatio: number): number {
  const sizeScore = proximityScore(fontSizeRatio, 1.0, 0.2);
  const hasNumberedEntry = /^\[?\d+\]?/.test(block.text.trim());
  return clamp01(0.45 + sizeScore * 0.25 + (hasNumberedEntry ? 0.2 : 0));
}

function scoreFigureCaption(block: Block): number {
  const text = block.text.trim();
  const match = /^(?:Fig(?:ure)?\.?\s*(\d+)|Fig\.\s*(\d+))/i.exec(text);
  if (!match) return 0.4;
  const hasNumber = match[1] || match[2] ? 1 : 0.7;
  const lengthScore = text.length >= 10 && text.length <= 300 ? 1 : 0.6;
  return clamp01(0.5 + hasNumber * 0.25 + lengthScore * 0.25);
}

function scoreTableCaption(block: Block): number {
  const text = block.text.trim();
  const match = /^Table\.?\s*(\d+)/i.exec(text);
  if (!match) return 0.4;
  const hasNumber = match[1] ? 1 : 0.7;
  const lengthScore = text.length >= 8 && text.length <= 250 ? 1 : 0.6;
  return clamp01(0.5 + hasNumber * 0.25 + lengthScore * 0.25);
}

function scoreEquation(block: Block, fontSizeRatio: number, hasMathFont: boolean): number {
  const text = block.text.trim();
  const mathFontRatio = hasMathFont
    ? block.lines.reduce((sum, l) => {
        const mathItems = l.items.filter(it => MATH_FONTS.some(mf => it.fontName.includes(mf))).length;
        return sum + (l.items.length ? mathItems / l.items.length : 0);
      }, 0) / Math.max(block.lines.length, 1)
    : 0;
  const symbolDensity = (text.match(/[=+\-*/∑∫∏√∂∇∞∈∉⊂⊃∪∩∧∨¬→⇒⇔∀∃]/g) || []).length / Math.max(text.length, 1);
  const mathSignal = clamp01(mathFontRatio + symbolDensity * 5);
  const centered = block.isCentered ? 1 : 0.6;
  const lineScore = block.lineCount <= 2 ? 1 : 0.7;
  const sizeScore = fontSizeRatio < 1.3 ? 1 : 0.6;
  return clamp01(0.4 + mathSignal * 0.35 + centered * 0.1 + lineScore * 0.1 + sizeScore * 0.05);
}

function scoreHeading(block: Block, fontSizeRatio: number): number {
  const text = block.text.trim();
  const numbered = HEADING_NUM_RE.test(text) ? 1 : 0.5;
  const centered = block.isCentered ? 0.9 : 1; // centered less typical for headings
  const sizeScore = clamp01((fontSizeRatio - 1.0) / 0.6); // 1.0→0, 1.6→1
  const len = text.length;
  const lengthScore = len >= 5 && len <= 120 ? 1 : 0.5;
  const lineScore = block.lineCount <= 2 ? 1 : 0.6;
  return clamp01(0.4 + numbered * 0.25 + sizeScore * 0.2 + lengthScore * 0.1 + lineScore * 0.05 + centered * 0.05);
}

function scoreBody(block: Block, fontSizeRatio: number): number {
  const sizeScore = proximityScore(fontSizeRatio, 1.0, 0.2);
  const lineScore = clamp01(block.lineCount / 3);
  return clamp01(0.45 + sizeScore * 0.3 + lineScore * 0.25);
}

function classifyBlocks(blocks: Block[], ctx: ClassifyContext): DetectedRegion[] {
  const regions: DetectedRegion[] = [];
  const isFirstPage = ctx.pageIndex === 1;
  // isLastPage reserved for future use (e.g. references detection)

  // Pre-compute centered flag
  for (const b of blocks) {
    const midX = (b.xMin + b.xMax) / 2;
    b.isCentered = Math.abs(midX - ctx.pageWidth / 2) < ctx.pageWidth * 0.2;
  }

  let titleFound = false;
  let abstractFound = false;
  let refsFound = false;

  for (const block of blocks) {
    const fontSizeRatio = block.maxFontSize / Math.max(ctx.bodyFontSize, 1);
    const textTrim = block.text.trim();

    // ── Title (first page, largest font in top 30%) ──
    if (isFirstPage && !titleFound && block.yTop < ctx.pageHeight * 0.3 && fontSizeRatio >= 1.35) {
      regions.push(makeRegion(block, 'title', ctx, scoreTitle(block, ctx, fontSizeRatio)));
      titleFound = true;
      continue;
    }

    // ── Authors (first page, after title, before abstract, medium font) ──
    if (isFirstPage && titleFound && !abstractFound && fontSizeRatio >= 1.1 && fontSizeRatio < 1.55) {
      // Check patterns: short lines, comma-separated, emails, affiliations
      const looksLikeAuthors =
        block.lineCount <= 4 &&
        /[,;†*‡§¶@]/.test(textTrim) &&
        !ABSTRACT_HEADING_RE.test(textTrim);
      if (looksLikeAuthors) {
        regions.push(makeRegion(block, 'authors', ctx, scoreAuthors(block, fontSizeRatio)));
        continue;
      }
    }

    // ── Abstract heading ──
    if (isFirstPage && !abstractFound && ABSTRACT_HEADING_RE.test(textTrim.split(/\s+/)[0])) {
      regions.push(makeRegion(block, 'abstract', ctx, scoreAbstractHeading(block)));
      abstractFound = true;
      continue;
    }

    // ── Abstract body (first page, text after abstract heading, body-sized font) ──
    if (isFirstPage && abstractFound && Math.abs(fontSizeRatio - 1.0) < 0.2 && block.lineCount >= 2) {
      regions.push(makeRegion(block, 'abstract', ctx, scoreAbstractBody(block, fontSizeRatio)));
      continue;
    }

    // ── References heading + body ──
    if (!refsFound && REFS_HEADING_RE.test(textTrim.split(/\s+/)[0])) {
      regions.push(makeRegion(block, 'references', ctx, scoreReferencesHeading(block)));
      refsFound = true;
      continue;
    }
    if (refsFound && Math.abs(fontSizeRatio - 1.0) < 0.25) {
      regions.push(makeRegion(block, 'references', ctx, scoreReferencesBody(block, fontSizeRatio)));
      continue;
    }

    // ── Figure / Table captions ──
    if (FIGURE_CAPTION_RE.test(textTrim)) {
      regions.push(makeRegion(block, 'figure', ctx, scoreFigureCaption(block)));
      continue;
    }
    if (TABLE_CAPTION_RE.test(textTrim)) {
      regions.push(makeRegion(block, 'table', ctx, scoreTableCaption(block)));
      continue;
    }

    // ── Equation: math font or single centered short line ──
    const hasMathFont = block.lines.some(l =>
      l.items.some(it => MATH_FONTS.some(mf => it.fontName.includes(mf)))
    );
    if (hasMathFont || (block.isCentered && block.lineCount <= 2 && fontSizeRatio < 1.2 &&
        /[=+\-*/∑∫∏√∂∇∞∈∉⊂⊃∪∩∧∨¬→⇒⇔∀∃]/.test(textTrim))) {
      regions.push(makeRegion(block, 'equation', ctx, scoreEquation(block, fontSizeRatio, hasMathFont)));
      continue;
    }

    // ── Heading: font 1.1x-1.8x body, short (1-2 lines), possibly numbered ──
    if (fontSizeRatio >= 1.1 && fontSizeRatio < 1.8 && block.lineCount <= 2 &&
        (HEADING_NUM_RE.test(textTrim) || block.isCentered || textTrim.length < 80)) {
      regions.push(makeRegion(block, 'heading', ctx, scoreHeading(block, fontSizeRatio)));
      continue;
    }

    // ── Body text ──
    if (Math.abs(fontSizeRatio - 1.0) < 0.25 && block.lineCount >= 1) {
      regions.push(makeRegion(block, 'body', ctx, scoreBody(block, fontSizeRatio)));
      continue;
    }

    // ── Fallback ──
    regions.push(makeRegion(block, 'unknown', ctx, 0.25));
  }

  return regions;
}

function makeRegion(block: Block, type: DetectedRegion['type'], ctx: ClassifyContext, confidence: number): DetectedRegion {
  const padding = 4;
  return {
    id: `${type}-${ctx.pageIndex}-${Math.round(block.yTop)}`,
    type,
    pageIndex: ctx.pageIndex,
    yRatio: Math.max(0, (block.yTop - padding) / ctx.pageHeight),
    xRatio: Math.max(0, (block.xMin - padding) / ctx.pageWidth),
    heightRatio: Math.min(1, (block.yBottom - block.yTop + padding * 2) / ctx.pageHeight),
    widthRatio: Math.min(1, (block.xMax - block.xMin + padding * 2) / ctx.pageWidth),
    text: block.text,
    confidence: clamp01(confidence),
  };
}

// ────────────────────────────────────────────────
// 5. Post-processing
// ────────────────────────────────────────────────

/** Merge adjacent same-type regions and remove overlaps. */
function postProcess(regions: DetectedRegion[]): DetectedRegion[] {
  if (regions.length <= 1) return regions;

  const merged: DetectedRegion[] = [];
  let current = { ...regions[0] };

  for (let i = 1; i < regions.length; i++) {
    const next = regions[i];
    if (current.type === next.type && next.yRatio - (current.yRatio + current.heightRatio) < 0.02) {
      // Merge vertically
      current.heightRatio = next.yRatio + next.heightRatio - current.yRatio;
      current.text = (current.text || '') + '\n' + (next.text || '');
      current.confidence = Math.max(current.confidence, next.confidence);
    } else {
      merged.push(current);
      current = { ...next };
    }
  }
  merged.push(current);

  // Filter very tiny regions (< 2% of page height) that are likely noise
  return merged.filter(r => r.heightRatio >= 0.008 || r.type === 'equation');
}

// ────────────────────────────────────────────────
// 6. Public API
// ────────────────────────────────────────────────

/**
 * Detect structural regions from raw pdf.js text items for a single page.
 *
 * Strategy:
 * 1. Normalize items (flip Y coordinate, extract font info)
 * 2. Cluster into lines by Y proximity
 * 3. Cluster lines into blocks by vertical gap
 * 4. Classify each block using font size, position, and text patterns
 * 5. Post-process: merge adjacent same-type regions
 *
 * Works on academic paper PDFs. Pure function — no side effects.
 */
export function detectRegionsFromItems(
  rawItems: { str: string; transform: number[]; height?: number; fontName?: string; width?: number }[],
  pageIndex: number,
  pageWidth: number,
  pageHeight: number,
  totalPages: number,
): DetectedRegion[] {
  if (rawItems.length === 0) return [];

  const items = normalizeItems(rawItems, pageHeight);
  const lines = clusterLines(items, pageWidth);
  if (lines.length === 0) return [];

  const blocks = clusterBlocks(lines, pageWidth);
  if (blocks.length === 0) return [];

  // Determine body font size: find the most common font size among blocks,
  // weighted by line count (more lines = more likely to be body text).
  // Build a histogram with 1px buckets, weighted by line count.
  const sizeHistogram = new Map<number, number>();
  for (const b of blocks) {
    const bucket = Math.round(b.maxFontSize);
    sizeHistogram.set(bucket, (sizeHistogram.get(bucket) || 0) + b.lineCount);
  }
  let bestSize = blocks.length > 0 ? blocks[0].maxFontSize : 12;
  let bestWeight = 0;
  for (const [size, weight] of sizeHistogram) {
    if (weight > bestWeight) {
      bestWeight = weight;
      bestSize = size;
    }
  }
  const bodyFontSize = bestSize;

  const ctx: ClassifyContext = {
    pageIndex,
    pageWidth,
    pageHeight,
    totalPages,
    bodyFontSize,
  };

  let regions = classifyBlocks(blocks, ctx);
  regions = postProcess(regions);

  return regions;
}

/**
 * Convenience wrapper that returns full page result.
 */
export function detectPageRegions(
  rawItems: { str: string; transform: number[]; height?: number; fontName?: string; width?: number }[],
  pageIndex: number,
  pageWidth: number,
  pageHeight: number,
  totalPages: number,
): PageRegionResult {
  const regions = detectRegionsFromItems(rawItems, pageIndex, pageWidth, pageHeight, totalPages);
  const avgConfidence = regions.length > 0
    ? regions.reduce((s, r) => s + r.confidence, 0) / regions.length
    : 0;
  return { pageIndex, regions, pageWidth, pageHeight, avgConfidence };
}
