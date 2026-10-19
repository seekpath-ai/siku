import { expect, test } from '@playwright/test';
import type { ClassifyRun } from '../src/lib/tauri';

interface ClassifyMockCall {
  cmd: string;
  args: Record<string, unknown>;
}

declare global {
  interface Window {
    __classifyMock: { calls: ClassifyMockCall[] };
  }
}

const DONE_RUN: ClassifyRun = {
  id: 'run-1',
  dimension: '研究主题',
  target: 'collection',
  scopeKind: 'selection',
  scopeId: null,
  sampleSize: 10,
  paperCount: 2,
  threshold: 0.75,
  twoLevel: true,
  skipExisting: false,
  groups: [
    { name: '深度学习', description: '神经网络方法', existingId: null, children: [] },
    { name: '实证研究', description: '', existingId: null, children: [] },
  ],
  items: [
    {
      paperId: 'p1',
      title: '文献一',
      year: 2025,
      groupName: '深度学习',
      labels: ['深度学习'],
      confidence: 0.9,
      reason: '命中主题',
      status: 'accepted',
      edited: false,
    },
    {
      paperId: 'p2',
      title: '文献二',
      year: 2024,
      groupName: '',
      labels: [],
      confidence: 0.4,
      reason: '',
      status: 'pending',
      edited: false,
    },
  ],
  status: 'done',
  appliedAt: null,
  undoneAt: null,
};

async function readClassifyState(page: import('@playwright/test').Page) {
  return page.evaluate(async () => {
    const { useAiClassifyStore } = await import('/src/stores/aiClassifyStore.ts');
    const s = useAiClassifyStore.getState();
    return { open: s.open, runId: s.runId, paperIds: s.paperIds };
  });
}

async function openClassifyWithRun(page: import('@playwright/test').Page) {
  await page.evaluate(async () => {
    const { useAiClassifyStore } = await import('/src/stores/aiClassifyStore.ts');
    useAiClassifyStore.getState().setRunId('run-1');
    useAiClassifyStore.getState().openWith(['p1', 'p2']);
  });
}

test.describe('AI 文献分类 跨页面/重开恢复', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript((run: ClassifyRun) => {
      localStorage.setItem('siku.onboarding.completed', '1');
      const calls: ClassifyMockCall[] = [];
      const w = window as unknown as Record<string, unknown>;
      w.__classifyMock = { calls };
      w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => undefined };
      w.__TAURI_INTERNALS__ = {
        metadata: { currentWindow: { label: 'main' } },
        transformCallback: () => 1,
        unregisterCallback: () => undefined,
        invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
          calls.push({ cmd, args });
          if (cmd === 'settings_get_all') {
            return [{ key: 'siku.onboarding.completed', value: '1', updated_at: '' }];
          }
          if (cmd === 'plugin:event|listen') return 1;
          if (cmd === 'llm_provider_list') return [];
          if (cmd === 'ai_classify_capabilities') return { semantic: false };
          if (cmd === 'ai_classify_vocab_list') return [];
          if (cmd === 'ai_classify_run_get') return run;
          if (cmd === 'ai_classify_start') return 'run-new';
          // Route-switch target (/graph) needs non-null payloads.
          if (cmd === 'graph_get') return { nodes: [], edges: [] };
          if (cmd === 'notes_list_all') return [];
          return null;
        },
      };
    }, DONE_RUN);
  });

  test('关闭对话框不清 runId，重新打开接回草案而不是重新开始', async ({ page }) => {
    await page.goto('/library', { waitUntil: 'networkidle' });
    await openClassifyWithRun(page);

    // Attached run restores straight into the review step.
    await expect(page.getByText('分类草案')).toBeVisible();
    await expect(page.getByText('文献一')).toBeVisible();

    // Closing keeps the attachment.
    await page.locator('button[title="关闭"]').click();
    await expect(page.getByText('分类草案')).toHaveCount(0);
    const closed = await readClassifyState(page);
    expect(closed.open).toBe(false);
    expect(closed.runId).toBe('run-1');

    // Reopening re-attaches to the same run — no new batch is started.
    await page.evaluate(async () => {
      const { useAiClassifyStore } = await import('/src/stores/aiClassifyStore.ts');
      useAiClassifyStore.getState().openWith(['p1', 'p2']);
    });
    await expect(page.getByText('分类草案')).toBeVisible();
    await expect(page.getByText('文献一')).toBeVisible();

    const calls = await page.evaluate(() => window.__classifyMock.calls);
    expect(calls.filter((c) => c.cmd === 'ai_classify_start')).toHaveLength(0);
    expect(calls.filter((c) => c.cmd === 'ai_classify_run_get').length).toBeGreaterThanOrEqual(2);
  });

  test('切换路由不重置对话框：状态仍在且不重新开始', async ({ page }) => {
    await page.goto('/library', { waitUntil: 'networkidle' });
    await openClassifyWithRun(page);
    await expect(page.getByText('分类草案')).toBeVisible();

    // 对话框全局挂载在 AppShell：切路由不卸载、不重置。
    await page.evaluate(async () => {
      const w = window as Window & { __sikuRouter?: { navigate: (o: { to: string }) => Promise<unknown> } };
      await w.__sikuRouter?.navigate({ to: '/graph' });
    });
    await page.waitForTimeout(800);
    expect(page.url()).toContain('/graph');
    await expect(page.getByText('分类草案')).toBeVisible();

    await page.evaluate(async () => {
      const w = window as Window & { __sikuRouter?: { navigate: (o: { to: string }) => Promise<unknown> } };
      await w.__sikuRouter?.navigate({ to: '/library' });
    });
    await page.waitForTimeout(800);
    expect(page.url()).toContain('/library');
    await expect(page.getByText('分类草案')).toBeVisible();
    await expect(page.getByText('文献一')).toBeVisible();

    const state = await readClassifyState(page);
    expect(state.runId).toBe('run-1');
    const calls = await page.evaluate(() => window.__classifyMock.calls);
    expect(calls.filter((c) => c.cmd === 'ai_classify_start')).toHaveLength(0);
    // 路由切换没有触发重复恢复。
    expect(calls.filter((c) => c.cmd === 'ai_classify_run_get')).toHaveLength(1);
  });

  test('只有用户明确开始新一轮时才丢弃旧 runId', async ({ page }) => {
    await page.goto('/library', { waitUntil: 'networkidle' });
    await openClassifyWithRun(page);
    await expect(page.getByText('分类草案')).toBeVisible();

    await page.getByText('放弃本轮，开始新一轮分类').click();
    await page.getByText('确认放弃本轮并重新开始？').click();

    // Detached: back to a blank config, runId cleared.
    await expect(page.getByRole('button', { name: /开始起草分类/ })).toBeVisible();
    const detached = await readClassifyState(page);
    expect(detached.runId).toBeNull();

    await page.getByRole('button', { name: /开始起草分类/ }).click();
    await expect
      .poll(async () => (await readClassifyState(page)).runId)
      .toBe('run-new');

    const calls = await page.evaluate(() => window.__classifyMock.calls);
    expect(calls.filter((c) => c.cmd === 'ai_classify_start')).toHaveLength(1);
  });
});
