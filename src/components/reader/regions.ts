/** Semantic region types for academic paper structure detection. */
export type RegionType =
  | 'title'
  | 'authors'
  | 'abstract'
  | 'body'
  | 'heading'
  | 'figure'
  | 'table'
  | 'equation'
  | 'references'
  | 'unknown';

/** A detected structural region on a single page. */
export interface DetectedRegion {
  id: string;
  type: RegionType;
  pageIndex: number;
  /** Bounding box as ratios of page dimensions (0-1). */
  yRatio: number;
  xRatio: number;
  heightRatio: number;
  widthRatio: number;
  /** Extracted text content of this region. */
  text?: string;
  /** Detection confidence 0-1. */
  confidence: number;
}

/** Rich metadata for a single pdf.js text item. */
export interface TextItemWithPosition {
  str: string;
  /** X offset in px (from page left edge). */
  x: number;
  /** Y offset in px (from page top edge, flipped from PDF coords). */
  y: number;
  /** Font size in px. */
  fontSize: number;
  /** Font name from pdf.js (e.g. "g_d0_f1"). */
  fontName: string;
  /** Item width in px. */
  width: number;
}

/** Detection result for a single page. */
export interface PageRegionResult {
  pageIndex: number;
  regions: DetectedRegion[];
  pageWidth: number;
  pageHeight: number;
  /** Average confidence of the rule-based detector (0-1). Low values suggest LLM refinement may help. */
  avgConfidence: number;
}

/** Color scheme for region type labels. */
export const REGION_COLORS: Record<RegionType, { bg: string; border: string; label: string }> = {
  title:     { bg: 'rgba(59, 130, 246, 0.12)', border: 'rgba(59, 130, 246, 0.4)',  label: '标题' },
  authors:   { bg: 'rgba(6, 182, 212, 0.10)',  border: 'rgba(6, 182, 212, 0.35)',   label: '作者' },
  abstract:  { bg: 'rgba(16, 185, 129, 0.10)',  border: 'rgba(16, 185, 129, 0.35)',  label: '摘要' },
  body:      { bg: 'rgba(148, 163, 184, 0.06)', border: 'rgba(148, 163, 184, 0.25)', label: '正文' },
  heading:   { bg: 'rgba(245, 158, 11, 0.10)',  border: 'rgba(245, 158, 11, 0.35)',  label: '章节' },
  figure:    { bg: 'rgba(236, 72, 153, 0.08)',  border: 'rgba(236, 72, 153, 0.30)',  label: '图' },
  table:     { bg: 'rgba(139, 92, 246, 0.08)',  border: 'rgba(139, 92, 246, 0.30)',  label: '表' },
  equation:  { bg: 'rgba(249, 115, 22, 0.08)',  border: 'rgba(249, 115, 22, 0.30)',  label: '公式' },
  references:{ bg: 'rgba(100, 116, 139, 0.10)',  border: 'rgba(100, 116, 139, 0.30)', label: '参考文献' },
  unknown:   { bg: 'rgba(148, 163, 184, 0.05)', border: 'rgba(148, 163, 184, 0.20)', label: '?' },
};

/** Request payload for LLM region detection (sent to backend). */
export interface LlmRegionRequest {
  page: number;
  pageWidth: number;
  pageHeight: number;
  totalPages: number;
  items: {
    str: string;
    x: number;
    y: number;
    fontSize: number;
    fontName: string;
    /** Item width in px from pdf.js (advance width of the text string). */
    width: number;
  }[];
}
