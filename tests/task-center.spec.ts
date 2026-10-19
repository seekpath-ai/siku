import { expect, test } from '@playwright/test';

type EventHandler = (event: unknown) => void;

declare global {
  interface Window {
    __taskCenterMock: {
      emit: (event: string, payload: Record<string, unknown>) => void;
    };
  }
}

/** Same mock Tauri IPC + event registry shape as ai-tasks.spec.ts, plus a
 *  hanging `import_papers_batch` so the batch import stays "running". */
async function setupLibrary(page: import('@playwright/test').Page) {
  await page.addInitScript(() => {
    localStorage.setItem('siku.onboarding.completed', '1');
    const callbacks = new Map<number, EventHandler>();
    const listeners = new Map<number, { event: string; handler: number }>();
    let nextCallbackId = 1;
    let nextEventId = 1;
    const w = window as unknown as Record<string, unknown>;
    w.__taskCenterMock = {
      emit: (event: string, payload: Record<string, unknown>) => {
        for (const [eventId, listener] of listeners) {
          if (listener.event !== event) continue;
          callbacks.get(listener.handler)?.({ event, id: eventId, payload });
        }
      },
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
        // Never resolves: the batch keeps "running" so minimize can be tested.
        if (cmd === 'import_papers_batch') return new Promise(() => {});
        if (cmd === 'library_cancel_batch_import') return null;
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
        if (cmd === 'notes_list_all' || cmd === 'notes_list' || cmd === 'notes_get_backlinks') return [];
        return null;
      },
    };
  });
  await page.goto('/library', { waitUntil: 'networkidle' });
}

const reviewPayload = (stage: string, extra: Record<string, unknown> = {}) => ({
  runId: 'r1',
  stage,
  sectionIndex: 2,
  totalSections: 5,
  sectionTitle: '研究进展',
  noteId: null,
  message: null,
  sectionStatus: 'running',
  doneSections: 2,
  ...extra,
});

const batchPayload = (current: number) => ({
  batchId: 'files', total: 3, current, file: `/p${current}.pdf`,
  lastStatus: 'imported', error: null, done: false, cancelled: false,
  imported: current, skipped: 0, failed: 0,
});

test.describe('任务中心（侧栏入口 + 面板）', () => {
  test('进度事件出现角标，面板列出任务，点击恢复综述对话框，清除已结束任务', async ({ page }) => {
    await setupLibrary(page);

    const button = page.getByTestId('task-center-button');
    const badge = page.getByTestId('task-center-badge');
    const panel = page.getByTestId('task-center-panel');
    await expect(badge).toBeHidden();

    // 空面板。
    await button.click();
    await expect(panel).toBeVisible();
    await expect(panel.getByText('暂无任务')).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(panel).toBeHidden();

    // 两个任务运行中 → 角标计数 2。
    await page.evaluate((payload) => {
      window.__taskCenterMock.emit('library:review-progress', payload);
      window.__taskCenterMock.emit('library:ai-classify-progress', {
        stage: 'assign', total: 20, done: 8, failed: 0, current: '', groupCount: 0, finished: null,
      });
    }, reviewPayload('section'));
    await expect(badge).toHaveText('2');

    await button.click();
    await expect(panel).toBeVisible();
    const reviewItem = page.getByTestId('task-center-item-review');
    const classifyItem = page.getByTestId('task-center-item-classify');
    await expect(reviewItem).toContainText('AI 综述');
    await expect(reviewItem).toContainText('综述 2/5');
    await expect(classifyItem).toContainText('分类判定 8/20');

    // 点击综述条目：恢复对话框，面板关闭。运行中的记录保留——关闭对话框后
    // 必须立即回到面板，不能干等下一条进度事件。
    await reviewItem.click();
    await expect(page.getByText('AI 综述', { exact: true })).toBeVisible();
    await expect(panel).toBeHidden();
    await page.getByTitle('关闭', { exact: true }).click();
    await button.click();
    await expect(page.getByTestId('task-center-item-review')).toContainText('综述 2/5');
    await expect(classifyItem).toBeVisible();

    // 综述结束后再点击：终态记录才在打开时移除。
    await page.evaluate((payload) => {
      window.__taskCenterMock.emit('library:review-progress', payload);
    }, reviewPayload('done'));
    await expect(page.getByTestId('task-center-item-review')).toContainText('综述 已完成');
    await page.getByTestId('task-center-item-review').click();
    await expect(page.getByText('AI 综述', { exact: true })).toBeVisible();
    await page.getByTitle('关闭', { exact: true }).click();
    await button.click();
    await expect(page.getByTestId('task-center-item-review')).toBeHidden();

    // 分类任务结束：记录保留（不占用角标），可清除。
    await page.evaluate(() => {
      window.__taskCenterMock.emit('library:ai-classify-progress', {
        stage: 'assign', total: 20, done: 20, failed: 0, current: '', groupCount: 0, finished: 'done',
      });
    });
    await expect(classifyItem).toContainText('分类 已完成 · 待审阅');
    await expect(badge).toBeHidden();
    await page.getByTestId('task-center-clear').click();
    await expect(classifyItem).toBeHidden();
    await expect(panel.getByText('暂无任务')).toBeVisible();
  });

  test('表格任务可从面板恢复；文献集为空时置灰', async ({ page }) => {
    await setupLibrary(page);

    // 无 paperIds：条目置灰且不可点击恢复。
    await page.evaluate(() => {
      window.__taskCenterMock.emit('library:ai-table-progress', {
        total: 10, done: 3, failed: 0, currentTitle: '测试文献一', finished: null,
      });
    });
    await page.getByTestId('task-center-button').click();
    const tableItem = page.getByTestId('task-center-item-table');
    await expect(tableItem).toBeDisabled();
    await page.keyboard.press('Escape');

    // 有 paperIds：点击恢复表格对话框。
    await page.evaluate(async () => {
      const { useAiTableStore } = await import('/src/stores/aiTableStore.ts');
      useAiTableStore.getState().openWith(['p1', 'p2']);
      useAiTableStore.getState().close();
    });
    await page.getByTestId('task-center-button').click();
    await expect(tableItem).toBeEnabled();
    await tableItem.click();
    await expect(page.getByText('AI 表格列', { exact: true })).toBeVisible();
    await expect(page.getByText('2 篇文献', { exact: true })).toBeVisible();
  });

  test('批量导入运行中可最小化，从任务中心恢复且进度保留', async ({ page }) => {
    await setupLibrary(page);

    await page.evaluate(async () => {
      const { runFileBatch } = await import('/src/stores/batchImportStore.ts');
      runFileBatch(['/a.pdf', '/b.pdf', '/c.pdf']).catch(() => {});
    });
    await expect(page.getByText('批量导入', { exact: true })).toBeVisible();

    const emitProgress = (current: number) =>
      page.evaluate(
        (payload) => window.__taskCenterMock.emit('library:batch-import-progress', payload),
        batchPayload(current)
      );
    await emitProgress(1);
    await expect(page.getByText('1 / 3', { exact: true })).toBeVisible();

    // 运行中最小化：对话框隐藏，后台任务继续，角标出现。
    await page.getByText('最小化', { exact: true }).click();
    await expect(page.getByText('批量导入', { exact: true })).toBeHidden();
    await expect(page.getByTestId('task-center-badge')).toHaveText('1');

    // 最小化后进度继续推进，面板反映最新进度。
    await emitProgress(2);
    await page.getByTestId('task-center-button').click();
    const item = page.getByTestId('task-center-item-batchImport');
    await expect(item).toContainText('批量导入');
    await expect(item).toContainText('导入 2/3');

    // 从面板恢复：对话框重新打开且进度保留。
    await item.click();
    await expect(page.getByText('批量导入', { exact: true })).toBeVisible();
    await expect(page.getByText('2 / 3', { exact: true })).toBeVisible();
    await expect(page.getByText('取消（当前篇会完成）', { exact: true })).toBeVisible();
  });
});
