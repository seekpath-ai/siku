import { expect, test } from '@playwright/test';

test('file URL 在 Markdown 安全转换前归一化为本地路径', async ({ page }) => {
  await page.goto('/library', { waitUntil: 'domcontentloaded' });
  const results = await page.evaluate(async () => {
    const { markdownUrlTransform } = await import('/src/components/chat/markdownConfig.ts');
    const cases = [
      {
        input: 'file:///C:/Users/Test%20User/Desktop/a%20b.png',
        expected: 'C:/Users/Test User/Desktop/a b.png',
      },
      {
        input: 'file:///C%3A%5CUsers%5CTest%5Ca.png',
        expected: 'C:\\Users\\Test\\a.png',
      },
      { input: 'file:///tmp/a%20b.png', expected: '/tmp/a b.png' },
      { input: 'file://localhost/tmp/a.png', expected: '/tmp/a.png' },
      { input: 'file://server/share/a.png', expected: '\\\\server\\share\\a.png' },
      { input: 'C:/Users/Test/a.png', expected: 'C:/Users/Test/a.png' },
      { input: 'https://example.com/a.png', expected: 'https://example.com/a.png' },
    ];
    return cases.map(({ input, expected }) => ({
      input,
      expected,
      actual: markdownUrlTransform(input),
    }));
  });

  for (const result of results) {
    expect(result.actual, result.input).toBe(result.expected);
  }
});

test('流式图片语法闭合后 file URL 不消失', async ({ page }) => {
  await page.route(/^(asset|http):\/\/(localhost|asset\.localhost)\//, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: 'image/png',
      body: Buffer.from(
        'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=',
        'base64',
      ),
    });
  });
  await page.goto('/library', { waitUntil: 'domcontentloaded' });
  await page.evaluate(async () => {
    const w = window as unknown as {
      __TAURI_INTERNALS__?: Record<string, unknown>;
      renderMarkdown?: (content: string) => void;
    };
    w.__TAURI_INTERNALS__ = {
      ...(w.__TAURI_INTERNALS__ ?? {}),
      convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}`,
    };
    const { default: React } = await import('/node_modules/.vite/deps/react.js');
    const { default: ReactDOMClient } = await import('/node_modules/.vite/deps/react-dom_client.js');
    const { StreamingContent } = await import('/src/components/chat/StreamingContent.tsx');
    const container = document.createElement('div');
    container.id = 'markdown-url-test';
    document.body.appendChild(container);
    const root = ReactDOMClient.createRoot(container);
    w.renderMarkdown = (content) => {
      root.render(React.createElement(StreamingContent, { content }));
    };
    w.renderMarkdown('![示例](file:///tmp/siku%20test.png');
  });

  const container = page.locator('#markdown-url-test');
  await expect(container).toContainText('file:///tmp/siku%20test.png');
  await page.evaluate(() => {
    const w = window as unknown as { renderMarkdown: (content: string) => void };
    w.renderMarkdown('![示例](file:///tmp/siku%20test.png)');
  });

  const image = container.getByRole('img', { name: '示例' });
  await expect(image).toBeVisible();
  await expect(image).toHaveAttribute('src', /asset/);
  await expect(container).not.toContainText('file:///tmp/siku%20test.png');
});
