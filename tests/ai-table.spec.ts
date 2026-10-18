import { expect, test } from '@playwright/test';

test.describe('AI 表格列设置', () => {
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

  test('多列时通过 Tab 切换并保留编辑状态', async ({ page }) => {
    await page.goto('/library', { waitUntil: 'networkidle' });
    await page.evaluate(async () => {
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
          if (cmd === 'plugin:event|listen') return 1;
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
          return null;
        },
      };
      const { useAiTableStore } = await import('/src/stores/aiTableStore.ts');
      useAiTableStore.getState().openWith(['p1', 'p2']);
    });

    const columnsTab = page.getByRole('tab', { name: '列定义', exact: true });
    const settingsTab = page.getByRole('tab', { name: '提取设置', exact: true });
    await expect(columnsTab).toHaveAttribute('aria-selected', 'true');
    await expect(settingsTab).toHaveAttribute('aria-selected', 'false');

    const columnInputs = page.getByPlaceholder('列名（如：样本量）');
    await columnInputs.first().fill('研究方法与技术路线（一个较长的列名）');
    for (let index = 1; index < 8; index += 1) {
      await page.getByRole('button', { name: '添加列' }).click();
      await columnInputs.nth(index).fill(`第 ${index + 1} 个较长的数据提取列名`);
    }
    await expect(columnInputs).toHaveCount(8);

    const scrolling = await page.locator('#ai-table-panel-columns > section').evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    }));
    expect(scrolling.scrollHeight).toBeGreaterThan(scrolling.clientHeight);

    await settingsTab.click();
    await expect(settingsTab).toHaveAttribute('aria-selected', 'true');
    await expect(page.getByText('设置本次抽取的深度、内容详略、交叉验证和模型。')).toBeVisible();
    await expect(columnInputs).toHaveCount(0);
    const detailSelect = page.locator('#ai-table-panel-settings select').nth(1);
    await detailSelect.selectOption('rich');
    await expect(detailSelect).toHaveValue('rich');

    await columnsTab.click();
    await expect(columnsTab).toHaveAttribute('aria-selected', 'true');
    await expect(columnInputs).toHaveCount(8);
    await expect(columnInputs.first()).toHaveValue('研究方法与技术路线（一个较长的列名）');

    await columnsTab.press('ArrowRight');
    await expect(settingsTab).toBeFocused();
    await expect(settingsTab).toHaveAttribute('aria-selected', 'true');
    await expect(page.locator('#ai-table-panel-settings select').nth(1)).toHaveValue('rich');

    const gridArea = page.locator('#ai-table-grid-area');
    const collapsedWidth = (await gridArea.boundingBox())?.width ?? 0;
    expect(collapsedWidth).toBeGreaterThan(0);
    await page.getByRole('button', { name: '模型推理 / 过程日志' }).click();
    const runDetails = page.locator('#ai-table-run-details');
    await expect(runDetails).toHaveAttribute('aria-hidden', 'false');
    await expect
      .poll(async () => collapsedWidth - ((await gridArea.boundingBox())?.width ?? 0))
      .toBeGreaterThan(280);
    await page.getByTitle('收起运行详情').click();
    await expect(runDetails).toHaveAttribute('aria-hidden', 'true');
    await expect
      .poll(async () => Math.abs(((await gridArea.boundingBox())?.width ?? 0) - collapsedWidth))
      .toBeLessThan(1);
  });
});
