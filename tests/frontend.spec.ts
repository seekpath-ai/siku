import { test, expect } from '@playwright/test';

const BASE_URL = 'http://localhost:1420';

test.describe('思库 E2E', () => {
  test.beforeEach(async ({ page }) => {
    page.on('pageerror', (err) => console.error(`[Page Error] ${err.message}`));
    page.on('console', (msg) => {
      if (msg.type() === 'error') console.error(`[Browser Error] ${msg.text()}`);
    });
    // Onboarding is now persisted via localStorage; keep it completed in tests
    // so it does not block navigation or add skip-onboarding delays.
    await page.addInitScript(() => {
      try {
        localStorage.setItem('siku.onboarding.completed', '1');
      } catch { /* ignore */ }
    });
  });

  // ===== 基础加载 =====
  test('页面加载无异常 + 标题正确', async ({ page }) => {
    const errors: string[] = [];
    page.on('pageerror', (err) => errors.push(err.message));
    page.on('console', (msg) => {
      if (msg.type() === 'error') errors.push(msg.text());
    });

    await page.goto(BASE_URL, { waitUntil: 'networkidle' });
    await expect(page).toHaveTitle('思库 · 让灵感涌动');
    expect(errors.filter(e => !e.includes('asset.localhost'))).toEqual([]);
  });

  // ===== 导航结构 =====
  test('侧边栏 7 个主导航项全部可见', async ({ page }) => {
    await page.goto(BASE_URL, { waitUntil: 'networkidle' });

    const sidebar = page.locator('aside.w-11');
    await expect(sidebar).toBeVisible();

    const navItems = ['图书馆', '对话', '知识库', '科研追踪', '笔记', '知识图谱', '文件列表'];
    for (const item of navItems) {
      await expect(sidebar.locator(`a[title="${item}"]`)).toBeVisible();
    }
  });

  test('导航跳转正确', async ({ page }) => {
    await page.goto(BASE_URL, { waitUntil: 'networkidle' });

    const sidebar = page.locator('aside.w-11');
    const routes: Record<string, string> = {
      '图书馆': '/library',
      '知识库': '/knowledge',
      '文件列表': '/files',
    };

    for (const [label, path] of Object.entries(routes)) {
      await sidebar.locator(`a[title="${label}"]`).click();
      await page.waitForTimeout(800);
      expect(page.url()).toContain(path);
    }
  });

  // ===== 空状态页面 =====
  test('图书馆空状态', async ({ page }) => {
    await page.goto(BASE_URL + '/library', { waitUntil: 'networkidle' });
    await page.waitForTimeout(3000);

    // Without Tauri backend: error state or empty state, or loading skeleton
    const hasEmpty = await page.getByText('还没有导入任何文献').isVisible().catch(() => false);
    const hasError = await page.getByText('加载文献失败').isVisible().catch(() => false);
    const hasSkeleton = await page.locator('.animate-pulse').isVisible().catch(() => false);
    expect(hasEmpty || hasError || hasSkeleton).toBeTruthy();
  });

  test('知识库页面加载', async ({ page }) => {
    await page.goto(BASE_URL + '/knowledge', { waitUntil: 'load' });
    await page.waitForTimeout(3000);
    // Page should render without crashing
    const title = await page.title();
    expect(title).toBe('思库 · 让灵感涌动');
  });

  test('科研追踪页面加载', async ({ page }) => {
    await page.goto(BASE_URL + '/research', { waitUntil: 'load' });
    await page.waitForTimeout(3000);
    // Page should render without crashing
    const title = await page.title();
    expect(title).toBe('思库 · 让灵感涌动');
  });

  test('笔记页面加载', async ({ page }) => {
    await page.goto(BASE_URL + '/notes', { waitUntil: 'networkidle' });
    await page.waitForTimeout(2000);

    await expect(page.getByText('文件列表')).toBeVisible();
    await expect(page.locator('button[title="新建笔记"]').first()).toBeVisible();
  });

  test('知识图谱页面加载', async ({ page }) => {
    await page.goto(BASE_URL + '/graph', { waitUntil: 'networkidle' });
    await page.waitForTimeout(3000);

    const hasGraph = await page.locator('canvas').isVisible().catch(() => false);
    const hasEmpty = await page.getByText('暂无图谱数据').isVisible().catch(() => false);
    expect(hasGraph || hasEmpty).toBeTruthy();
  });

  test('文件浏览器页面加载', async ({ page }) => {
    await page.goto(BASE_URL + '/files', { waitUntil: 'networkidle' });
    await page.waitForTimeout(2000);

    // Should show the path navigator
    const hasHome = await page.locator('button').first().isVisible();
    expect(hasHome).toBeTruthy();
  });

  // ===== 对话页面 =====
  test('对话页面加载 + 新建会话', async ({ page }) => {
    await page.goto(BASE_URL + '/chat', { waitUntil: 'networkidle' });
    await page.waitForTimeout(2000);

    const hasNewAgent = await page.getByText('新建智能体').isVisible().catch(() => false);
    const hasAgentSection = await page.getByText('智能体').isVisible().catch(() => false);
    const hasProjectFiles = await page.getByText('项目文件').isVisible().catch(() => false);
    expect(hasNewAgent || hasAgentSection || hasProjectFiles).toBeTruthy();
  });

  test('设置页面加载', async ({ page }) => {
    await page.goto(BASE_URL + '/settings', { waitUntil: 'networkidle' });
    await page.waitForTimeout(2000);

    // Sidebar categories
    await expect(page.getByRole('button', { name: '通用', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '模型提供商', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: '智能体默认值', exact: true })).toBeVisible();
    // Default active panel
    await expect(page.getByRole('heading', { name: '通用设置', exact: true })).toBeVisible();
  });

  // ===== 暗色主题 =====
  test('暗色主题 CSS 变量正确', async ({ page }) => {
    await page.goto(BASE_URL, { waitUntil: 'networkidle' });

    const bgColor = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--background').trim());
    expect(bgColor).toBe('#1A1A1E');

    const primary = await page.evaluate(() =>
      getComputedStyle(document.documentElement).getPropertyValue('--primary').trim());
    expect(primary).toBe('#E67E22');
  });

  // ===== 截图 =====
  test('各页面截图', async ({ page }) => {
    const pages = ['/library', '/chat', '/knowledge', '/notes', '/graph', '/files', '/settings'];
    for (const path of pages) {
      await page.goto(BASE_URL + path, { waitUntil: 'networkidle' });
        await page.waitForTimeout(1500);
      await page.screenshot({
        path: `tests/screenshots/${path.replace(/\//g, '_')}.png`,
        fullPage: true,
      });
    }
  });
});
