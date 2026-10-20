import { expect, test } from '@playwright/test';

const NOTE = {
  id: 'n1',
  vault_id: 'v1',
  title: '围栏测试',
  content: '',
  content_plain: '',
  tags: '[]',
  aliases: '[]',
  is_favorite: 0,
  is_folder: 0,
  is_system: 0,
  is_locked: 0,
  source_collection_id: null,
  is_excerpt: 0,
  agent_edited_at: null,
  agent_edit_count: 0,
  paper_id: null,
  parent_id: null,
  sort_order: 0,
  is_literature_note: 0,
  created_at: '2026-01-01T00:00:00Z',
  updated_at: '2026-01-01T00:00:00Z',
};

type InvokeArgs = Record<string, unknown>;

/** Installs the mock Tauri IPC (same shape as ai-tasks.spec.ts) with one empty
 *  note, and opens it directly via /notes?note=n1. */
async function setupNoteEditor(page: import('@playwright/test').Page, note: typeof NOTE) {
  await page.addInitScript((initialNote) => {
    localStorage.setItem('siku.onboarding.completed', '1');
    const callbacks = new Map<number, (event: unknown) => void>();
    let nextCallbackId = 1;
    const vault = { id: 'v1', name: '测试库', created_at: '', updated_at: '' };
    let current = initialNote;
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
      invoke: async (cmd: string, args: InvokeArgs = {}) => {
        if (cmd === 'settings_get_all') {
          return [{ key: 'siku.onboarding.completed', value: '1', updated_at: '' }];
        }
        if (cmd === 'plugin:event|listen') return nextCallbackId++;
        if (cmd === 'plugin:event|unlisten') return null;
        if (cmd === 'llm_provider_list') return [];
        if (cmd === 'notes_list_all' || cmd === 'notes_list') return [current];
        if (cmd === 'notes_get_backlinks') return [];
        if (cmd === 'notes_update') {
          current = {
            ...current,
            title: (args.title as string | undefined) ?? current.title,
            content: (args.content as string | undefined) ?? current.content,
          };
          return current;
        }
        if (cmd === 'vault_list') return [vault];
        if (cmd === 'vault_current') return vault;
        if (cmd === 'vault_attachments_dir') return '/tmp';
        if (cmd === 'files_list') return [];
        return null;
      },
    };
  }, note);
  await page.goto(`/notes?note=${note.id}`, { waitUntil: 'networkidle' });
  const content = page.locator('.cm-content');
  await expect(content).toBeVisible();
  return content;
}

/** Full document text of the first CodeMirror editor, joined from .cm-line
 *  elements (DOM order = document order). */
function docText(page: import('@playwright/test').Page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll('.cm-line')).map((l) => l.textContent ?? '').join('\n')
  );
}

test.describe('代码块围栏自动补全', () => {
  test('行首凑齐 ``` 时自动补全闭合围栏，光标落在中间空行，一次撤销回退整个展开', async ({ page }) => {
    await setupNoteEditor(page, NOTE);

    await page.locator('.cm-content').click();
    // Pause past CM history's newGroupDelay (500ms) before the third backtick,
    // so undo grouping isolates "third backtick + expansion" as one event.
    await page.locator('.cm-content').pressSequentially('``');
    await page.waitForTimeout(700);
    await page.locator('.cm-content').pressSequentially('`');

    await expect.poll(() => docText(page)).toBe('```\n\n```');
    // Caret on the middle empty line (status bar shows 行 2, 列 1).
    await expect(page.getByText('行 2, 列 1', { exact: true })).toBeVisible();

    // One Ctrl+Z undoes the expansion AND the third backtick together.
    await page.keyboard.press('Control+z');
    await expect.poll(() => docText(page)).toBe('``');
  });

  test('已有代码块内部输入 ``` 不触发补全', async ({ page }) => {
    await setupNoteEditor(page, NOTE);

    await page.locator('.cm-content').click();
    await page.locator('.cm-content').pressSequentially('```');
    await expect.poll(() => docText(page)).toBe('```\n\n```');

    // Caret is on the middle line inside the block; typing ``` there closes
    // the block instead of expanding a new one.
    await page.locator('.cm-content').pressSequentially('```');
    await expect.poll(() => docText(page)).toBe('```\n```\n```');
    await expect(page.locator('.cm-line')).toHaveCount(3);
  });

  test('行内文本中间输入 ``` 不触发补全', async ({ page }) => {
    await setupNoteEditor(page, { ...NOTE, content: 'abc' });

    await page.locator('.cm-content').click();
    await page.keyboard.press('End');
    await page.locator('.cm-content').pressSequentially('```');

    await expect.poll(() => docText(page)).toBe('abc```');
    await expect(page.locator('.cm-line')).toHaveCount(1);
  });
});
