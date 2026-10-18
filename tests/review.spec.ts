import { expect, test } from '@playwright/test';
import type {
  ReviewPaperCard,
  ReviewProgress,
  ReviewSection,
  ReviewSnapshot,
  ReviewSource,
} from '../src/lib/tauri';

interface ReviewMockCall {
  cmd: string;
  args: Record<string, unknown>;
}

interface ReviewMock {
  snapshot: ReviewSnapshot;
  calls: ReviewMockCall[];
  emit: (stage: string, overrides?: Partial<ReviewProgress>) => void;
  finishReading: () => void;
  listenerCount: () => number;
}

declare global {
  interface Window {
    __reviewMock: ReviewMock;
  }
}

const PAPER_IDS = ['p1', 'p2', 'p3', 'p4'];

test.describe('AI 综述四步流程', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem('siku.onboarding.completed', '1');
      let nextCallbackId = 1;
      const callbacks = new Map<number, (event: unknown) => void>();
      const w = window as unknown as Record<string, unknown>;
      w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => undefined };
      w.__TAURI_INTERNALS__ = {
        metadata: { currentWindow: { label: 'main' } },
        transformCallback: (callback: (event: unknown) => void) => {
          const id = nextCallbackId++;
          callbacks.set(id, callback);
          return id;
        },
        unregisterCallback: (id: number) => callbacks.delete(id),
        invoke: async (cmd: string) => {
          if (cmd === 'settings_get_all') {
            return [{ key: 'siku.onboarding.completed', value: '1', updated_at: '' }];
          }
          if (cmd === 'plugin:event|listen') return 1;
          return null;
        },
      };
    });
  });

  test('阅读、恢复、大纲、写作审计与保存', async ({ page }) => {
    await page.goto('/library', { waitUntil: 'networkidle' });

    await page.evaluate(async (paperIds) => {
      type InvokeArgs = Record<string, unknown>;
      type EventHandler = (event: unknown) => void;
      type TauriTestWindow = Window & {
        __TAURI_INTERNALS__: {
          metadata: { currentWindow: { label: string } };
          invoke: (cmd: string, args?: InvokeArgs) => Promise<unknown>;
          transformCallback: (callback: EventHandler) => number;
          unregisterCallback: (id: number) => void;
        };
        __TAURI_EVENT_PLUGIN_INTERNALS__: {
          unregisterListener: (event: string, id: number) => void;
        };
      };

      const w = window as TauriTestWindow;
      const callbacks = new Map<number, EventHandler>();
      const listeners = new Map<number, { event: string; handler: number }>();
      let nextCallbackId = 1;
      let nextEventId = 1;

      const makeCard = (paperId: string, sourceType: 'fulltext' | 'abstract', evidenceId: string): ReviewPaperCard => ({
        paperId,
        researchQuestion: `${paperId} 的研究问题`,
        methods: sourceType === 'fulltext' ? '对照实验' : '摘要所述方法',
        dataAndSample: '公开数据集',
        findings: [{ claim: `${paperId} 的主要发现`, evidenceIds: [evidenceId] }],
        limitations: '样本范围有限',
        relevance: '直接支持综述主题',
        sourceType,
        edited: false,
        evidence: [
          {
            id: evidenceId,
            chunkId: sourceType === 'fulltext' ? `chunk-${paperId}` : null,
            page: sourceType === 'fulltext' ? 3 : null,
            quote: `${paperId} 的原文证据`,
            content: `${paperId} 的证据上下文`,
          },
        ],
      });

      const sources: ReviewSource[] = [
        {
          paperId: 'p1',
          paperIndex: 1,
          title: '全文研究',
          authors: '["作者甲"]',
          year: 2025,
          sourceKind: 'fulltext',
          status: 'pending',
          chunkCount: 4,
          abstractChars: 120,
          card: null,
          error: null,
        },
        {
          paperId: 'p2',
          paperIndex: 2,
          title: '摘要研究',
          authors: '["作者乙"]',
          year: 2024,
          sourceKind: 'abstract',
          status: 'pending',
          chunkCount: 0,
          abstractChars: 360,
          card: null,
          error: null,
        },
        {
          paperId: 'p3',
          paperIndex: 3,
          title: '失败研究',
          authors: '[]',
          year: 2023,
          sourceKind: 'fulltext',
          status: 'pending',
          chunkCount: 2,
          abstractChars: 80,
          card: null,
          error: null,
        },
        {
          paperId: 'p4',
          paperIndex: 4,
          title: '无内容研究',
          authors: '[]',
          year: null,
          sourceKind: 'unusable',
          status: 'done',
          chunkCount: 0,
          abstractChars: 0,
          card: null,
          error: null,
        },
      ];

      const snapshot: ReviewSnapshot = {
        runId: 'run-1',
        status: 'created',
        stage: 'read',
        question: '',
        scope: '',
        language: '中文',
        targetWords: 5000,
        providerId: null,
        paperIds,
        sources,
        outline: [],
        synthesis: null,
        sections: [],
        finalRawMarkdown: null,
        audit: null,
        noteId: null,
        error: null,
      };

      const mock: ReviewMock = {
        snapshot,
        calls: [],
        finishReading: () => completeInitialReading(),
        listenerCount: () => listeners.size,
        emit: (stage, overrides = {}) => {
          const payload: ReviewProgress = {
            runId: 'run-1',
            stage,
            sectionIndex: 0,
            totalSections: 0,
            sectionTitle: '',
            noteId: null,
            message: null,
            ...overrides,
          };
          for (const [eventId, listener] of listeners) {
            if (listener.event !== 'library:review-progress') continue;
            callbacks.get(listener.handler)?.({ event: listener.event, id: eventId, payload });
          }
        },
      };
      w.__reviewMock = mock;

      const completeInitialReading = () => {
        mock.snapshot.sources[0] = {
          ...mock.snapshot.sources[0],
          status: 'done',
          card: makeCard('p1', 'fulltext', 'E1'),
        };
        mock.snapshot.sources[1] = {
          ...mock.snapshot.sources[1],
          status: 'done',
          card: makeCard('p2', 'abstract', 'E2'),
        };
        mock.snapshot.sources[2] = {
          ...mock.snapshot.sources[2],
          status: 'failed',
          error: '模型暂时不可用',
        };
        mock.snapshot.status = 'read_ready';
        mock.snapshot.stage = 'outline';
        mock.snapshot.synthesis = {
          themes: [{ title: '方法演进', detail: '全文与摘要共同指向方法演进。', paperIds: ['p1', 'p2'] }],
          agreements: [],
          conflicts: [],
          gaps: [{ title: '覆盖不足', detail: '仍有文献阅读失败。', paperIds: ['p3'] }],
        };
      };

      const simulateGeneration = async () => {
        const pause = () => new Promise((resolve) => setTimeout(resolve, 40));
        for (let index = 0; index < mock.snapshot.outline.length; index += 1) {
          const section = mock.snapshot.outline[index];
          mock.snapshot.status = 'generating';
          mock.snapshot.stage = 'section';
          mock.snapshot.sections[index] = {
            ...mock.snapshot.sections[index],
            status: 'running',
            attempts: 1,
          };
          mock.emit('section', {
            sectionIndex: index + 1,
            totalSections: mock.snapshot.outline.length,
            sectionTitle: section.title,
            sectionStatus: 'running',
            attempt: 0,
            maxTokens: 4096,
          });
          await pause();
          const rawMarkdown = `## ${section.title}\n\n本节依据证据展开论述。[E${index + 1}]`;
          mock.emit('section', {
            sectionIndex: index + 1,
            totalSections: mock.snapshot.outline.length,
            sectionTitle: section.title,
            delta: rawMarkdown,
          });
          mock.snapshot.sections[index] = {
            ...mock.snapshot.sections[index],
            status: 'done',
            rawMarkdown,
          };
          mock.emit('section', {
            sectionIndex: index + 1,
            totalSections: mock.snapshot.outline.length,
            sectionTitle: section.title,
            sectionStatus: 'done',
            doneSections: index + 1,
          });
          await pause();
        }

        mock.snapshot.stage = 'revision';
        mock.emit('revision', { delta: '统一术语与过渡，但不改变引用序列。' });
        await pause();
        mock.snapshot.status = 'done';
        mock.snapshot.stage = 'done';
        mock.snapshot.finalRawMarkdown = mock.snapshot.sections.map((section) => section.rawMarkdown).join('\n\n');
        mock.snapshot.audit = {
          selectedCount: 4,
          fulltextCount: 2,
          abstractCount: 1,
          unusableCount: 1,
          citedPaperIds: ['p1', 'p2'],
          uncitedPaperIds: ['p3'],
          invalidMarkers: ['[E99]'],
          warnings: ['全文修订未通过引用序列保护，已保留原始分节稿。', '无内容研究没有可用全文或摘要。'],
        };
        mock.emit('done', { totalSections: mock.snapshot.outline.length });
      };

      w.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
        unregisterListener: (event, eventId) => {
          listeners.delete(eventId);
        },
      };
      w.__TAURI_INTERNALS__ = {
        metadata: { currentWindow: { label: 'main' } },
        transformCallback: (callback) => {
          const id = nextCallbackId++;
          callbacks.set(id, callback);
          return id;
        },
        unregisterCallback: (id) => {
          callbacks.delete(id);
        },
        invoke: async (cmd, args = {}) => {
          mock.calls.push({ cmd, args });
          if (cmd === 'llm_provider_list') return [];
          if (cmd === 'plugin:event|listen') {
            const eventId = nextEventId++;
            listeners.set(eventId, { event: String(args.event), handler: Number(args.handler) });
            return eventId;
          }
          if (cmd === 'plugin:event|unlisten') {
            listeners.delete(Number(args.eventId));
            return null;
          }
          if (cmd === 'review_create') {
            mock.snapshot.question = String(args.question);
            mock.snapshot.scope = String(args.scope);
            mock.snapshot.language = String(args.language);
            mock.snapshot.targetWords = Number(args.targetWords);
            mock.snapshot.providerId = args.providerId ? String(args.providerId) : null;
            return mock.snapshot;
          }
          if (cmd === 'review_get') return mock.snapshot;
          if (cmd === 'review_read_start') {
            mock.snapshot.status = 'reading';
            mock.snapshot.stage = 'read';
            return null;
          }
          if (cmd === 'review_read_retry') {
            const paperId = String(args.paperId);
            const index = mock.snapshot.sources.findIndex((source) => source.paperId === paperId);
            mock.snapshot.sources[index] = {
              ...mock.snapshot.sources[index],
              status: 'done',
              error: null,
              card: makeCard(paperId, 'fulltext', 'E3'),
            };
            mock.snapshot.status = 'read_ready';
            mock.snapshot.stage = 'outline';
            mock.snapshot.synthesis = {
              themes: [{ title: '综合主题', detail: '三篇可用文献已完成综合。', paperIds: ['p1', 'p2', 'p3'] }],
              agreements: [{ title: '共同结论', detail: '方法需要结合数据质量评估。', paperIds: ['p1', 'p3'] }],
              conflicts: [],
              gaps: [],
            };
            return null;
          }
          if (cmd === 'review_generate_outline') {
            mock.snapshot.outline = [
              { title: '研究进展', points: '比较主要方法与发现', subsections: [], paperIds: ['p1', 'p2'] },
              { title: '局限与展望', points: '讨论局限和未来方向', subsections: [], paperIds: ['p3'] },
            ];
            mock.snapshot.status = 'read_ready';
            mock.snapshot.stage = 'outline';
            return mock.snapshot.outline;
          }
          if (cmd === 'review_generate_start') {
            mock.snapshot.outline = args.outline as ReviewSection[];
            mock.snapshot.sections = mock.snapshot.outline.map((_, sectionIndex) => ({
              sectionIndex,
              status: 'pending',
              rawMarkdown: '',
              attempts: 0,
              error: null,
            }));
            mock.snapshot.status = 'generating';
            mock.snapshot.stage = 'section';
            mock.snapshot.finalRawMarkdown = null;
            mock.snapshot.audit = null;
            setTimeout(() => void simulateGeneration(), 20);
            return null;
          }
          if (cmd === 'review_generate_save') {
            mock.snapshot.noteId = 'note-1';
            mock.snapshot.status = 'done';
            mock.snapshot.stage = 'done';
            return 'note-1';
          }
          return null;
        },
      };

      const { useReviewStore } = await import('/src/stores/reviewStore.ts');
      useReviewStore.getState().openWith(paperIds);
    }, PAPER_IDS);

    await expect(page.getByRole('button', { name: '开始阅读' })).toBeVisible();
    await page.getByPlaceholder('如：大语言模型在软件工程任务中的应用进展如何？').fill('如何综合评估现有研究方法？');
    await page.getByRole('button', { name: '开始阅读' }).click();

    await expect(page.getByText('阅读进度 1/4')).toBeVisible();
    await page.waitForFunction(() => window.__reviewMock.listenerCount() > 0);
    await page.evaluate(() => {
      window.__reviewMock.finishReading();
      window.__reviewMock.emit('read_ready', { donePapers: 4, totalPapers: 4 });
    });

    await expect(page.getByText('阅读进度 4/4')).toBeVisible();
    await expect(page.getByText('仅摘要', { exact: true })).toBeVisible();
    await expect(page.getByText('不可用', { exact: true }).first()).toBeVisible();
    await expect(page.getByText('模型暂时不可用')).toBeVisible();

    const abstractSource = page.locator('li', { hasText: '摘要研究' }).first();
    await abstractSource.getByRole('button').first().click();
    await expect(abstractSource).toContainText('基于摘要的证据卡');
    await expect(abstractSource).toContainText('无页码');
    await expect(abstractSource).not.toContainText('chunk:');

    const failedSource = page.locator('li', { hasText: '失败研究' }).first();
    await failedSource.getByRole('button', { name: '重试' }).click();
    await expect(failedSource).toContainText('完成');
    await expect(page.getByText('三篇可用文献已完成综合。')).toHaveCount(0);

    await page.getByTitle('关闭', { exact: true }).click();
    await expect(page.getByRole('button', { name: '进入大纲' })).toBeHidden();
    await page.evaluate(async (paperIds) => {
      const { useReviewStore } = await import('/src/stores/reviewStore.ts');
      useReviewStore.getState().openWith(paperIds);
    }, PAPER_IDS);
    await expect(page.getByText('跨文献综合', { exact: true })).toBeVisible();
    await expect(page.getByText('三篇可用文献已完成综合。')).toBeVisible();

    await page.getByRole('button', { name: '阅读检查' }).click();
    await expect(page.locator('li', { hasText: '失败研究' }).first()).toContainText('完成');
    await page.getByRole('button', { name: '进入大纲' }).click();
    await page.getByRole('button', { name: '生成大纲' }).click();
    const firstSectionTitle = page.getByPlaceholder('章节标题').first();
    await expect(firstSectionTitle).toHaveValue('研究进展');
    await firstSectionTitle.fill('研究进展（编辑后）');
    await page.getByRole('button', { name: '开始生成' }).click();

    await expect(page.getByText('引用与覆盖审计', { exact: true })).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText('实际引用（2）', { exact: true })).toBeVisible();
    await expect(page.getByText('未引用（1）', { exact: true })).toBeVisible();
    await expect(page.getByText('[E99]', { exact: true })).toBeVisible();
    await expect(page.getByText('全文修订未通过引用序列保护，已保留原始分节稿。')).toBeVisible();

    const generateCall = await page.evaluate(() =>
      window.__reviewMock.calls.find((call) => call.cmd === 'review_generate_start')
    );
    expect(generateCall).toBeDefined();
    const generatedOutline = generateCall?.args.outline as ReviewSection[];
    expect(generatedOutline[0]).toMatchObject({ title: '研究进展（编辑后）', paperIds: ['p1', 'p2'] });
    expect(generatedOutline[1].paperIds).toEqual(['p3']);

    await page.getByRole('button', { name: '保存综述' }).click();
    await expect(page.getByText('综述已保存为笔记，引用编号可点击溯源。')).toBeVisible();
    await expect(page.getByRole('button', { name: '打开笔记' })).toBeVisible();

    await page.getByTitle('关闭', { exact: true }).click();
    await page.evaluate(async (paperIds) => {
      const { useReviewStore } = await import('/src/stores/reviewStore.ts');
      useReviewStore.getState().openWith(paperIds);
    }, PAPER_IDS);
    await expect(page.getByText('引用与覆盖审计', { exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '打开笔记' })).toBeVisible();
  });
});
