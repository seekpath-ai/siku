import { expect, test } from "@playwright/test";

const paper = {
  id: "p1",
  title:
    "A Deep Dive into Large Language Models for Automated Bug Localization and Repair",
  authors: '["Alice","Bob","Carol","Dave","Eve"]',
  year: 2024,
  journal: "Proceedings of the ACM on Software Engineering",
  doi: "10.1145/3660773",
  url: "https://doi.org/10.1145/3660773",
  abstract_text: "用于验证文献信息阅读态的摘要。",
  keywords: '["LLM","软件工程"]',
  citation_key: "deep-dive-2024",
  bibtex: null,
  file_path: "/tmp/paper.pdf",
  file_size: 100,
  page_count: 23,
  language: "en",
  item_type: "journal",
  volume: "1",
  issue: "FSE",
  pages: "1471-1493",
  conference_name: null,
  publisher: "Association for Computing Machinery",
  place: null,
  editor: "[]",
  series: null,
  edition: null,
  isbn: null,
  issn: null,
  num_pages: null,
  archive_location: null,
  call_number: null,
  rights: null,
  deleted_at: null,
  is_favorite: 0,
  read_status: "unread",
  last_read_at: null,
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
  imported_at: "2026-01-01T00:00:00Z",
};

const creators = (JSON.parse(paper.authors) as string[]).map((name) => ({
  role: "author",
  name,
  first_name: "",
  last_name: "",
}));

test.describe("文献信息与元数据补全", () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(
      ({ paper, creators }) => {
        localStorage.setItem("siku.onboarding.completed", "1");
        let nextCallbackId = 1;
        let enrichCalls = 0;
        const callbacks = new Map<number, (event: unknown) => void>();
        const w = window as unknown as Record<string, unknown>;
        w.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
          unregisterListener: () => undefined,
        };
        w.__TAURI_INTERNALS__ = {
          metadata: { currentWindow: { label: "main" } },
          transformCallback: (callback: (event: unknown) => void) => {
            const id = nextCallbackId++;
            callbacks.set(id, callback);
            return id;
          },
          unregisterCallback: (id: number) => callbacks.delete(id),
          invoke: async (cmd: string) => {
            if (cmd === "settings_get_all") {
              return [
                {
                  key: "siku.onboarding.completed",
                  value: "1",
                  updated_at: "",
                },
              ];
            }
            if (cmd === "plugin:event|listen") return 1;
            if (cmd === "list_papers" || cmd === "get_paper")
              return cmd === "list_papers" ? [paper] : paper;
            if (cmd === "paper_get_creators") return creators;
            if (cmd === "paper_enrich_metadata") {
              enrichCalls += 1;
              w.__enrichCalls = enrichCalls;
              return { updated: true, fields: ["publisher", "year"] };
            }
            if (
              [
                "collections_list",
                "tags_list",
                "paper_get_tags",
                "paper_get_collections",
                "paper_list_related",
                "paper_list_attachments",
                "saved_searches_list",
                "llm_provider_list",
              ].includes(cmd)
            ) {
              return [];
            }
            return null;
          },
        };
        w.__enrichCalls = 0;
      },
      { paper, creators },
    );
  });

  test("默认阅读态支持作者折叠、空字段切换和取消编辑", async ({ page }) => {
    await page.goto("/library", { waitUntil: "networkidle" });
    await page.evaluate(async () => {
      const { useLibraryStore } = await import("/src/stores/libraryStore.ts");
      useLibraryStore.getState().selectPaper("p1");
      useLibraryStore.getState().setRightPanelWidth(400);
      useLibraryStore.getState().setRightPanelCollapsed(false);
    });

    await expect(
      page.getByRole("heading", { name: paper.title, exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "编辑", exact: true }),
    ).toBeVisible();
    await expect(page.getByLabel("标题")).toHaveCount(0);
    await expect(page.getByText("出版地", { exact: true })).toHaveCount(0);

    await page.getByRole("button", { name: "还有 2 位", exact: true }).click();
    await expect(page.getByText(/Eve/)).toBeVisible();
    await page
      .getByRole("button", { name: "显示全部字段", exact: true })
      .click();
    await expect(page.getByText("出版地", { exact: true })).toBeVisible();

    await page.getByRole("button", { name: "编辑", exact: true }).click();
    await expect(page.getByLabel("标题")).toHaveValue(paper.title);
    await page.getByRole("textbox", { name: "期刊", exact: true }).fill("Changed Journal");
    await page.getByRole("button", { name: "取消", exact: true }).click();
    await expect(
      page.getByRole("definition").filter({ hasText: paper.journal }),
    ).toBeVisible();
    await expect(
      page.getByText("Changed Journal", { exact: true }),
    ).toHaveCount(0);
  });

  test("矮窗口菜单保持可见且可重复手动补全", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 420 });
    await page.goto("/library", { waitUntil: "networkidle" });

    const title = page.getByText(paper.title, { exact: true }).first();
    await title.click({ button: "right" });
    const enrich = page.getByRole("button", {
      name: "补全元数据…",
      exact: true,
    });
    await expect(enrich).toBeVisible();
    const menuBox = await enrich.locator("..").boundingBox();
    expect(menuBox).not.toBeNull();
    expect(menuBox!.y).toBeGreaterThanOrEqual(8);
    expect(menuBox!.y + menuBox!.height).toBeLessThanOrEqual(412);

    await enrich.click();
    await expect(
      page.getByText("已补全：出版社、年份", { exact: true }),
    ).toBeVisible();
    await page.getByRole("button", { name: "确定", exact: true }).click();

    await title.click({ button: "right" });
    await page
      .getByRole("button", { name: "补全元数据…", exact: true })
      .click();
    await expect(
      page.getByText("已补全：出版社、年份", { exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => (window as unknown as { __enrichCalls: number }).__enrichCalls,
      ),
    ).toBe(2);
  });
});
