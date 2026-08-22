/**
 * Find-match highlighter for the PDF text layer.
 *
 * This is a TypeScript port of pdf.js v6 `web/text_highlighter.js`
 * (Apache-2.0, Copyright Mozilla Foundation). pdfjs-dist ships the class
 * inside its viewer bundle but does NOT export it, so the viewer cannot
 * instantiate it — this port restores match highlighting and
 * scroll-into-view for the in-app find bar.
 *
 * The class consumes the public PDFFindController accessors
 * (`pageMatches`, `selected`, `state`, `scrollMatchIntoView`, …) and listens
 * to `updatetextlayermatches` on the shared event bus, exactly like the
 * upstream original.
 */

interface FindControllerLike {
  pageMatches: (number[] | null)[];
  pageMatchesLength: (number[] | null)[];
  selected: { pageIdx: number; matchIdx: number };
  state: { highlightAll: boolean } | null;
  highlightMatches: boolean;
  scrollMatchIntoView(opts: {
    element: HTMLElement | null;
    pageIndex: number;
    matchIndex: number;
  }): void;
}

interface EventBusLike {
  on(eventName: string, listener: (evt: { pageIndex: number }) => void, options?: unknown): void;
}

interface MatchRange {
  begin: { divIdx: number; offset: number };
  end: { divIdx: number; offset: number };
}

export class TextHighlighter {
  matches: MatchRange[] = [];
  enabled = false;

  private textDivs: (HTMLElement | Text)[] | null = null;
  private textContentItemsStr: string[] | null = null;
  private eventAC: AbortController | null = null;

  constructor(
    private readonly findController: FindControllerLike,
    private readonly eventBus: EventBusLike,
    private readonly pageIdx: number,
  ) {}

  setTextMapping(divs: (HTMLElement | Text)[], texts: string[]) {
    this.textDivs = divs;
    this.textContentItemsStr = texts;
  }

  enable() {
    if (!this.textDivs || !this.textContentItemsStr) {
      throw new Error('Text divs and strings have not been set.');
    }
    if (this.enabled) {
      throw new Error('TextHighlighter is already enabled.');
    }
    this.enabled = true;
    if (!this.eventAC) {
      this.eventAC = new AbortController();
      this.eventBus.on(
        'updatetextlayermatches',
        (evt) => {
          if (evt.pageIndex === this.pageIdx || evt.pageIndex === -1) {
            this.updateMatches();
          }
        },
        { signal: this.eventAC.signal },
      );
    }
    this.updateMatches();
  }

  disable() {
    if (!this.enabled) return;
    this.enabled = false;
    this.eventAC?.abort();
    this.eventAC = null;
    this.updateMatches(true);
  }

  /** Map character-offset matches onto text-layer div offsets. */
  private convertMatches(matches: number[] | null, matchesLength: number[] | null): MatchRange[] {
    const { textContentItemsStr } = this;
    if (!matches || !matchesLength || !textContentItemsStr) return [];
    let i = 0;
    let iIndex = 0;
    const end = textContentItemsStr.length - 1;
    const result: MatchRange[] = [];
    for (let m = 0, mm = matches.length; m < mm; m++) {
      let matchIdx = matches[m];
      while (i !== end && matchIdx >= iIndex + textContentItemsStr[i].length) {
        iIndex += textContentItemsStr[i].length;
        i++;
      }
      if (i === textContentItemsStr.length) {
        console.error('Could not find a matching mapping');
      }
      const match: MatchRange = {
        begin: { divIdx: i, offset: matchIdx - iIndex },
        end: { divIdx: i, offset: 0 },
      };
      matchIdx += matchesLength[m];
      while (i !== end && matchIdx > iIndex + textContentItemsStr[i].length) {
        iIndex += textContentItemsStr[i].length;
        i++;
      }
      match.end = { divIdx: i, offset: matchIdx - iIndex };
      result.push(match);
    }
    return result;
  }

  private renderMatches(matches: MatchRange[]) {
    if (matches.length === 0) return;
    const { findController, pageIdx } = this;
    const textContentItemsStr = this.textContentItemsStr!;
    const textDivs = this.textDivs!;
    const isSelectedPage = pageIdx === findController.selected.pageIdx;
    const selectedMatchIdx = findController.selected.matchIdx;
    const highlightAll = findController.state?.highlightAll ?? false;
    let prevEnd: { divIdx: number; offset: number } | null = null;
    const infinity = { divIdx: -1, offset: undefined as unknown as number };

    const appendTextToDiv = (
      divIdx: number,
      fromOffset: number,
      toOffset: number,
      className?: string,
    ): HTMLElement | null => {
      let div = textDivs[divIdx] as HTMLElement;
      if (div.nodeType === Node.TEXT_NODE) {
        const span = document.createElement('span');
        (div as unknown as ChildNode).before?.(span);
        // textDivs entries may be raw text nodes; wrap them in a span once.
        span.append(div as unknown as Text);
        textDivs[divIdx] = span;
        div = span;
      }
      const content = textContentItemsStr[divIdx].substring(fromOffset, toOffset);
      const node = document.createTextNode(content);
      if (className) {
        const span = document.createElement('span');
        span.className = `${className} appended`;
        span.append(node);
        div.append(span);
        return className.includes('selected') ? span : null;
      }
      div.append(node);
      return null;
    };

    const beginText = (begin: { divIdx: number; offset: number }, className?: string) => {
      const divIdx = begin.divIdx;
      (textDivs[divIdx] as HTMLElement).textContent = '';
      return appendTextToDiv(divIdx, 0, begin.offset, className);
    };

    let i0 = selectedMatchIdx;
    let i1 = i0 + 1;
    if (highlightAll) {
      i0 = 0;
      i1 = matches.length;
    } else if (!isSelectedPage) {
      return;
    }
    let lastDivIdx = -1;
    let lastOffset = -1;
    for (let i = i0; i < i1; i++) {
      const match = matches[i];
      const begin = match.begin;
      if (begin.divIdx === lastDivIdx && begin.offset === lastOffset) {
        continue;
      }
      lastDivIdx = begin.divIdx;
      lastOffset = begin.offset;
      const end = match.end;
      const isSelected = isSelectedPage && i === selectedMatchIdx;
      const highlightSuffix = isSelected ? ' selected' : '';
      let selectedSpan: HTMLElement | null = null;
      if (!prevEnd || begin.divIdx !== prevEnd.divIdx) {
        if (prevEnd !== null) {
          appendTextToDiv(prevEnd.divIdx, prevEnd.offset, infinity.offset);
        }
        beginText(begin);
      } else {
        appendTextToDiv(prevEnd.divIdx, prevEnd.offset, begin.offset);
      }
      if (begin.divIdx === end.divIdx) {
        selectedSpan = appendTextToDiv(begin.divIdx, begin.offset, end.offset, 'highlight' + highlightSuffix);
      } else {
        selectedSpan = appendTextToDiv(begin.divIdx, begin.offset, infinity.offset, 'highlight begin' + highlightSuffix);
        for (let n0 = begin.divIdx + 1, n1 = end.divIdx; n0 < n1; n0++) {
          (textDivs[n0] as HTMLElement).className = 'highlight middle' + highlightSuffix;
        }
        beginText(end, 'highlight end' + highlightSuffix);
      }
      prevEnd = end;
      if (isSelected) {
        findController.scrollMatchIntoView({
          element: selectedSpan,
          pageIndex: pageIdx,
          matchIndex: selectedMatchIdx,
        });
      }
    }
    if (prevEnd) {
      appendTextToDiv(prevEnd.divIdx, prevEnd.offset, infinity.offset);
    }
  }

  updateMatches(reset = false) {
    if (!this.enabled && !reset) return;
    const { findController, matches, pageIdx } = this;
    const textContentItemsStr = this.textContentItemsStr!;
    const textDivs = this.textDivs!;
    let clearedUntilDivIdx = -1;
    for (const match of matches) {
      const begin = Math.max(clearedUntilDivIdx, match.begin.divIdx);
      for (let n = begin, end = match.end.divIdx; n <= end; n++) {
        const div = textDivs[n] as HTMLElement;
        div.textContent = textContentItemsStr[n];
        div.className = '';
      }
      clearedUntilDivIdx = match.end.divIdx + 1;
    }
    if (!findController?.highlightMatches || reset) return;
    const pageMatches = findController.pageMatches[pageIdx] || null;
    const pageMatchesLength = findController.pageMatchesLength[pageIdx] || null;
    this.matches = this.convertMatches(pageMatches, pageMatchesLength);
    this.renderMatches(this.matches);
  }
}
