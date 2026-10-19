import { expect, test } from '@playwright/test';

type EventHandler = (event: unknown) => void;

declare global {
  interface Window {
    __aiTasksMock: {
      emit: (event: string, payload: Record<string, unknown>) => void;
      listenerCount: () => number;
    };
  }
}

/** Installs the mock Tauri IPC + event registry (same shape as review.spec.ts)
 *  and returns once the page is on /library. */
async function setupLibrary(page: import('@playwright/test').Page) {
  await page.addInitScript(() => {
    localStorage.setItem('siku.onboarding.completed', '1');
    const callbacks = new Map<number, EventHandler>();
    const listeners = new Map<number, { event: string; handler: number }>();
    let nextCallbackId = 1;
    let nextEventId = 1;
    const w = window as unknown as Record<string, unknown>;
    w.__aiTasksMock = {
      emit: (event: string, payload: Record<string, unknown>) => {
        for (const [eventId, listener] of listeners) {
          if (listener.event !== event) continue;
          callbacks.get(listener.handler)?.({ event, id: eventId, payload });
        }
      },
      listenerCount: () => listeners.size,
    };
    w.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: (_event: string, eventId: number) => {
        listeners.delete(eventId);
      },
    };
    w.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: 'main' } },
      transformCallback: (callback: EventHandler) => {
        const id = nextCallbackId++;
        callbacks.set(id, callback);
        return id;
      },
      unregisterCallback: (id: number) => callbacks.delete(id),
      invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
        if (cmd === 'settings_get_all') {
          return [{ key: 'siku.onboarding.completed', value: '1', updated_at: '' }];
        }
        if (cmd === 'plugin:event|listen') {
          const eventId = nextEventId++;
          listeners.set(eventId, { event: String(args.event), handler: Number(args.handler) });
          return eventId;
        }
        if (cmd === 'plugin:event|unlisten') {
          listeners.delete(Number(args.eventId));
          return null;
        }
        if (cmd === 'llm_provider_list') return [];
        if (cmd === 'ai_table_papers_info') {
          return [
            { id: 'p1', title: '测试文献一', year: 2025 },
            { id: 'p2', title: '测试文献二', year: 2024 },
          ];
        }
        if (cmd === 'ai_table_extract_status') return { running: false, progress: null };
        if (
          cmd === 'ai_table_presets_list' ||
          cmd === 'ai_table_builtin_presets' ||
          cmd === 'ai_table_cells_list' ||
          cmd === 'ai_table_terms_get'
        ) {
          return [];
        }
        if (cmd === 'ai_classify_capabilities') return { semantic: false };
        if (cmd === 'ai_classify_vocab_list') return [];
        // The /notes route iterates these directly — null would crash it.
        if (cmd === 'notes_list_all' || cmd === 'notes_list' || cmd === 'notes_get_backlinks') return [];
        return null;
      },
    };
  });
  await page.goto('/library', { waitUntil: 'networkidle' });
}

test.describe('全局 AI 任务入口', () => {
  test('进度 chip 跨路由保留，点击重新打开对应对话框，完成后保留至打开', async ({ page }) => {
    await setupLibrary(page);

    // 表格先开再关：close 保留 paperIds，chip 的 reopen 才有目标。
    await page.evaluate(async () => {
      const { useAiTableStore } = await import('/src/stores/aiTableStore.ts');
      useAiTableStore.getState().openWith(['p1', 'p2']);
    });
    await expect(page.getByText('AI 表格列', { exact: true })).toBeVisible();
    await page.getByTitle('关闭', { exact: true }).click();
    await expect(page.getByText('AI 表格列', { exact: true })).toBeHidden();

    const reviewChip = page.getByTestId('ai-task-chip-review');
    const tableChip = page.getByTestId('ai-task-chip-table');
    const classifyChip = page.getByTestId('ai-task-chip-classify');
    await expect(reviewChip).toBeHidden();
    await expect(tableChip).toBeHidden();
    await expect(classifyChip).toBeHidden();

    await page.evaluate(() => {
      window.__aiTasksMock.emit('library:ai-table-progress', { total: 10, done: 3, failed: 0, currentTitle: '测试文献一', finished: null });
      window.__aiTasksMock.emit('library:review-progress', {
        runId: 'r1',
        stage: 'section',
        sectionIndex: 2,
        totalSections: 5,
        sectionTitle: '研究进展',
        noteId: null,
        message: null,
        sectionStatus: 'running',
        doneSections: 2,
      });
      window.__aiTasksMock.emit('library:ai-classify-progress', {
        stage: 'assign',
        total: 20,
        done: 8,
        failed: 0,
        current: '',
        groupCount: 0,
        finished: null,
      });
    });
    await expect(tableChip).toHaveText(/表格 3\/10/);
    await expect(reviewChip).toHaveText(/综述 2\/5/);
    await expect(classifyChip).toHaveText(/分类判定 8\/20/);

    // 切到 /notes 再回 /library：入口和状态不丢。
    await page.keyboard.press('Control+3');
    await expect(page).toHaveURL(/\/notes/);
    await expect(tableChip).toHaveText(/表格 3\/10/);
    await expect(reviewChip).toHaveText(/综述 2\/5/);
    await expect(classifyChip).toHaveText(/分类判定 8\/20/);

    // 在 /notes 上点击分类 chip：全局对话框直接打开（不重置其 store 状态）。
    await classifyChip.click();
    await expect(page.getByText('AI 文献分类', { exact: true })).toBeVisible();
    await expect(classifyChip).toBeHidden();
    await page.getByTitle('关闭', { exact: true }).click();
    await expect(page.getByText('AI 文献分类', { exact: true })).toBeHidden();

    await page.keyboard.press('Control+1');
    await expect(page).toHaveURL(/\/library/);
    await expect(tableChip).toBeVisible();
    await expect(reviewChip).toBeVisible();

    // 表格 chip → 重新打开表格对话框（papers 仍在）。
    await tableChip.click();
    await expect(page.getByText('AI 表格列', { exact: true })).toBeVisible();
    await expect(page.getByText('2 篇文献', { exact: true })).toBeVisible();
    await page.getByTitle('关闭', { exact: true }).click();

    // 综述完成后保留「已完成」chip，直到用户点击打开。
    await page.evaluate(() => {
      window.__aiTasksMock.emit('library:review-progress', {
        runId: 'r1',
        stage: 'done',
        sectionIndex: 0,
        totalSections: 5,
        sectionTitle: '',
        noteId: 'note-1',
        message: null,
      });
    });
    await expect(reviewChip).toHaveText(/综述 已完成/);
    await page.keyboard.press('Control+3');
    await expect(reviewChip).toHaveText(/综述 已完成/);
    await page.keyboard.press('Control+1');
    await expect(reviewChip).toHaveText(/综述 已完成/);
    await reviewChip.click();
    await expect(page.getByText('AI 综述', { exact: true })).toBeVisible();
    await expect(reviewChip).toBeHidden();
  });

  test('AI 表格关闭不停止任务且保留 paperIds，可从 chip 恢复', async ({ page }) => {
    await setupLibrary(page);

    await page.evaluate(async () => {
      const { useAiTableStore } = await import('/src/stores/aiTableStore.ts');
      useAiTableStore.getState().openWith(['p1', 'p2']);
    });
    await expect(page.getByText('AI 表格列', { exact: true })).toBeVisible();
    await expect(page.getByText('2 篇文献', { exact: true })).toBeVisible();

    // 关闭后任务在后台继续（进度事件仍会到达），chip 出现。
    await page.evaluate(() => {
      window.__aiTasksMock.emit('library:ai-table-progress', { total: 10, done: 3, failed: 0, currentTitle: '测试文献一', finished: null });
    });
    await page.getByTitle('关闭', { exact: true }).click();
    await expect(page.getByText('AI 表格列', { exact: true })).toBeHidden();

    // close 不清空 paperIds。
    const paperIds = await page.evaluate(async () => {
      const { useAiTableStore } = await import('/src/stores/aiTableStore.ts');
      return useAiTableStore.getState().paperIds;
    });
    expect(paperIds).toEqual(['p1', 'p2']);

    // 关闭后进度继续推进，chip 反映最新进度；点击 chip 恢复对话框。
    await page.evaluate(() => {
      window.__aiTasksMock.emit('library:ai-table-progress', { total: 10, done: 4, failed: 0, currentTitle: '测试文献二', finished: null });
    });
    const tableChip = page.getByTestId('ai-task-chip-table');
    await expect(tableChip).toHaveText(/表格 4\/10/);
    await tableChip.click();
    await expect(page.getByText('AI 表格列', { exact: true })).toBeVisible();
    await expect(page.getByText('2 篇文献', { exact: true })).toBeVisible();

    // 完成事件：关闭状态下 chip 变为「已完成」，点击后消失。
    await page.getByTitle('关闭', { exact: true }).click();
    await page.evaluate(() => {
      window.__aiTasksMock.emit('library:ai-table-progress', { total: 10, done: 10, failed: 0, currentTitle: '', finished: 'done' });
    });
    await expect(tableChip).toHaveText(/表格 已完成/);
    await tableChip.click();
    await expect(page.getByText('AI 表格列', { exact: true })).toBeVisible();
    await expect(tableChip).toBeHidden();
  });
});
