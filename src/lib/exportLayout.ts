// ── Export helpers for hybrid/manual region detection ──
// Replicates the backend's build_layout_prompt() logic in TypeScript
// so users can copy-paste page layout data into external LLM chat apps.
//
// SYNC WITH: src-tauri/src/ai/region_detection/service.rs (SYSTEM_PROMPT + build_layout_prompt)

import type { LlmRegionRequest } from '@/components/reader/regions';

// ── System prompt (mirrors service.rs SYSTEM_PROMPT) ──

export const SYSTEM_PROMPT = `Output a JSON array of structural regions found on this academic paper page. Do NOT write any analysis, reasoning, or explanation. Output ONLY the JSON array, starting with [ and ending with ].

Region types: title, authors, abstract, heading, body, figure, table, equation, references.

Rules:
- title: largest font, page 1, top 30%
- authors: after title, medium font, short lines with commas/emails
- abstract: after authors, starts with "Abstract" or body text following abstract label
- heading: font between title and body, 1-2 short lines, often numbered
- body: most common font size, long paragraphs
- figure: caption starting with "Fig" / "Figure"
- table: caption starting with "Table"
- equation: centered, math font names (CMSY/CMMI/MSAM) or math symbols
- references: starts with "References"/"Bibliography", small font, [1]-style entries

Output format (include "page" field for multi-page responses):
[{"page":1,"type":"title","yRatio":0.09,"xRatio":0.15,"heightRatio":0.04,"widthRatio":0.7,"confidence":0.95,"text":"Paper Title"}]

IMPORTANT coordinate convention: The y value in each LINE entry is the text BASELINE, NOT the visual top. The "font" value is the font size in px. To compute region boundaries: visual top = y - font (ascenders extend ~1×font above baseline); visual bottom = y (baseline, descenders are negligible). So yRatio = (y - font) / pageHeight, and heightRatio = (bottom_y - top_y) / pageHeight. For multi-line regions, use the visual top of the first line and the baseline of the last line.`;

// ── Raw item type (pdf.js TextItem shape) ──

export interface RawTextItem {
  str: string;
  transform: number[];
  height?: number;
  fontName?: string;
  width?: number;
}

interface LineEntry {
  y: number;
  xStart: number;
  xEnd: number;
  fontSize: number;
  text: string;
}

// ── Line clustering (matches backend: 3px Y tolerance) ──

function clusterItemsIntoLines(
  items: RawTextItem[],
  pageHeight: number,
  yTolerance: number = 3,
): LineEntry[] {
  // Convert to top-down coords with font info, skip empty
  const positioned = items
    .filter((it) => it.str?.trim())
    .map((it) => {
      const t = it.transform;
      return {
        str: it.str,
        x: t[4],
        y: pageHeight - t[5], // flip PDF bottom-up → top-down
        fontSize: it.height ?? Math.abs(t[3]) ?? 12,
        width: it.width ?? 0,  // actual advance width from pdf.js
      };
    });

  if (positioned.length === 0) return [];

  // Sort by Y, then X
  positioned.sort((a, b) => a.y - b.y || a.x - b.x);

  // Cluster into lines by Y proximity
  const rawLines: typeof positioned[] = [];
  for (const item of positioned) {
    let placed = false;
    for (const line of rawLines) {
      if (Math.abs(line[0].y - item.y) <= yTolerance) {
        line.push(item);
        placed = true;
        break;
      }
    }
    if (!placed) rawLines.push([item]);
  }

  // Build line entries
  const lines: LineEntry[] = [];
  for (const raw of rawLines) {
    raw.sort((a, b) => a.x - b.x);
    const text = raw.map((it) => it.str).join(' ');
    if (!text.trim()) continue;
    const xStart = raw[0].x;
    const last = raw[raw.length - 1];
    const fontSize = Math.max(...raw.map((it) => it.fontSize));
    // Use actual advance width from pdf.js, not a char-count estimate
    const xEnd = last.x + (last.width > 0 ? last.width : last.str.length * fontSize * 0.5);
    lines.push({
      y: raw.reduce((s, it) => s + it.y, 0) / raw.length,
      xStart,
      xEnd,
      fontSize,
      text,
    });
  }

  // Sort top→bottom
  lines.sort((a, b) => a.y - b.y);

  return lines;
}

// ── LINE-format text builder (mirrors build_layout_prompt) ──

export function buildPageLayoutText(
  items: RawTextItem[],
  pageNum: number,
  totalPages: number,
  pageWidth: number,
  pageHeight: number,
): string {
  const lines = clusterItemsIntoLines(items, pageHeight);

  let text = `PAGE ${pageNum} of ${totalPages} (width=${Math.round(pageWidth)}, height=${Math.round(pageHeight)})\n`;

  for (const line of lines) {
    const textShort = line.text.length > 200
      ? line.text.slice(0, 200) + '...'
      : line.text;

    text += `LINE y=${Math.round(line.y)} x=${Math.round(line.xStart)}-${Math.round(line.xEnd)} font=${Math.round(line.fontSize)}: "${textShort}"\n`;
  }

  return text;
}

// ── JSON export builder (matches LlmRegionRequest) ──

export function buildJsonExport(
  items: RawTextItem[],
  pageNum: number,
  totalPages: number,
  pageWidth: number,
  pageHeight: number,
): LlmRegionRequest {
  const exportItems: LlmRegionRequest['items'] = [];
  for (const item of items) {
    const s = item.str;
    if (!s?.trim()) continue;
    const t = item.transform;
    exportItems.push({
      str: s,
      x: t[4],
      y: pageHeight - t[5],
      fontSize: item.height ?? Math.abs(t[3]) ?? 12,
      fontName: item.fontName ?? '',
      width: item.width ?? 0,
    });
  }

  return {
    page: pageNum,
    pageWidth,
    pageHeight,
    totalPages,
    items: exportItems,
  };
}

// ── Page content fetcher type ──

export type PageContentFetcher = (pageNum: number) => Promise<{
  items: RawTextItem[];
  width: number;
  height: number;
} | null>;

// ── Combined export generator ──

export async function generateExportText(
  fetchPage: PageContentFetcher,
  startPage: number,
  endPage: number,
  totalPages: number,
): Promise<{ lineText: string; jsonText: string }> {
  const lineParts: string[] = [];
  const jsonRequests: LlmRegionRequest[] = [];

  for (let p = startPage; p <= endPage; p++) {
    const data = await fetchPage(p);
    if (!data) continue;

    // LINE format
    lineParts.push(
      buildPageLayoutText(data.items, p, totalPages, data.width, data.height),
    );

    // JSON format
    jsonRequests.push(
      buildJsonExport(data.items, p, totalPages, data.width, data.height),
    );
  }

  // Append system prompt to LINE text
  if (lineParts.length > 0) {
    lineParts.push(`\n--- 系统提示 ---\n${SYSTEM_PROMPT}`);
  }

  const lineText = lineParts.join('\n');
  const jsonText = JSON.stringify({
    systemPrompt: SYSTEM_PROMPT,
    pages: jsonRequests,
  }, null, 2);

  return { lineText, jsonText };
}
