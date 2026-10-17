/**
 * 导出 PDF 的「引用链接修正」验收脚本（无浏览器依赖）。
 *
 * 把 src/lib/noteCitations.ts 编译到临时目录，用仓库真实的 markdown 栈
 * （react-markdown + remark-gfm）渲染一份综述形态的笔记，断言：
 *   1. 正文引用改指向 #siku-ref-N，且深链不再残留；
 *   2. 每条参考文献各自成段、带 id 与 .pdf-ref-entry（打印时不首行缩进）；
 *   3. 同一篇论文的多处引用指向同一锚点；
 *   4. hasFixableCitations 对「有引用+参考文献」「无参考文献」「脚注式旧笔记」判断正确。
 *
 * 用法：node verify-citations.mjs
 */
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

const outDir = mkdtempSync(join(tmpdir(), 'siku-cite-'));
execFileSync('npx', ['tsc', 'src/lib/noteCitations.ts', '--module', 'esnext', '--target', 'es2022',
  '--moduleResolution', 'bundler', '--outDir', outDir], { stdio: 'pipe' });
const { toReferenceCitations, rehypeReferenceAnchors, hasFixableCitations, REF_ENTRY_CLASS } =
  await import(join(outDir, 'noteCitations.js'));
rmSync(outDir, { recursive: true, force: true });

const sample = [
  '## 第一节',
  '',
  '过程式工作流受固定步骤约束[3]，智能体式允许自主探索[[9]](siku-reader://p9?page=7&exact=x)[[13]](siku-reader://p13?page=2&exact=y)。',
  '同篇论文的第二次引用[[9]](siku-reader://p9?page=12&exact=z)。',
  '',
  '## 参考文献',
  '',
  '[3] 张三. 某研究. 学报, 2023.',
  '[9] 李四. 另一研究. 顶会, 2024.',
  '[13] Wang. Agentic SE. ICSE, 2025.',
].join('\n');

const html = renderToStaticMarkup(
  React.createElement(ReactMarkdown, {
    remarkPlugins: [remarkGfm],
    rehypePlugins: [[await import('rehype-katex').then((m) => m.default), { throwOnError: false }], rehypeReferenceAnchors],
  }, toReferenceCitations(sample))
);

const checks = [
  ['正文引用指向 #siku-ref-9', html.includes('href="#siku-ref-9"')],
  ['正文引用指向 #siku-ref-13', html.includes('href="#siku-ref-13"')],
  ['参考文献 [3] 有锚点 id', html.includes('id="siku-ref-3"')],
  ['参考文献 [9] 有锚点 id', html.includes('id="siku-ref-9"')],
  ['参考文献 [13] 有锚点 id', html.includes('id="siku-ref-13"')],
  [`参考文献带 .${REF_ENTRY_CLASS}（打印不缩进）`, html.includes(REF_ENTRY_CLASS)],
  ['深链未残留', !html.includes('siku-reader://')],
  ['同篇两处引用指向同一锚点', (html.match(/href="#siku-ref-9"/g) || []).length === 2],
  // Attribute order is not guaranteed (class may precede id) — count <p> tags
  // that carry a reference id at all.
  ['每条参考文献独占一段', (html.match(/<p[^>]*id="siku-ref-/g) || []).length === 3],
  ['识别：有引用+参考文献', hasFixableCitations(sample) === true],
  ['识别：无参考文献段', hasFixableCitations('正文[[9]](siku-reader://p9?page=1&exact=x)') === false],
  ['识别：普通笔记', hasFixableCitations('## 标题\n\n普通文字') === false],
];

let failed = 0;
for (const [name, ok] of checks) {
  if (!ok) failed += 1;
  console.log(`${ok ? 'OK  ' : 'FAIL'} ${name}`);
}
console.log(failed === 0 ? '\n✅ 引用链接修正全部通过' : `\n⚠️ ${failed} 项失败`);
process.exit(failed === 0 ? 0 : 1);
