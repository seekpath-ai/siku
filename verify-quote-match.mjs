/**
 * 引用定位（PDF 高亮）验收脚本 —— 无浏览器依赖。
 *
 * 复现用户报的问题：AI 综述里的引用点开后「跳到文献了但没高亮」，而文献分析
 * 智能体保存的笔记引用正常。根因是引用的 exact 来自我们自己的索引文本，它和
 * pdf.js 的 text layer 并不逐字相同：
 *   · 分块器把行尾断词接回去了（"depen-\ndencies" → "dependencies"），
 *     pdfium 甚至把那个断字报成 U+0002，而 text layer 里仍是一个连字符；
 *   · 双栏排版插入的空格、硬换行会把一句话切开；
 *   · 小型大写标题改变大小写。
 * 按原始字符比对时，真实论文里的长引用几乎必然跨过其中一处 → 定位失败 → 只剩
 * 跳页。src/lib/quoteMatch.ts 改成「归一化后比对 + 长引用退化为句首」，并把
 * 命中位置映射回原始字符串以便画高亮框。
 *
 * 用法：node verify-quote-match.mjs
 */
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const outDir = mkdtempSync(join(tmpdir(), 'siku-quote-'));
execFileSync(
  'npx',
  [
    'tsc',
    'src/lib/quoteMatch.ts',
    '--module',
    'esnext',
    '--target',
    'es2022',
    '--moduleResolution',
    'bundler',
    '--outDir',
    outDir,
  ],
  { stdio: 'pipe' }
);
const { normaliseForMatch, quoteNeedles, matchStrength, findQuote } = await import(
  join(outDir, 'quoteMatch.js')
);
rmSync(outDir, { recursive: true, force: true });

let failed = 0;
const check = (label, ok, detail = '') => {
  console.log(`${ok ? 'OK  ' : 'FAIL'} ${label}${ok || !detail ? '' : ` — ${detail}`}`);
  if (!ok) failed += 1;
};

/** 归一化后去掉空白/连字符/大小写，长度应保持一致的可映射性。 */
{
  const src = 'Depen-\ndencies  of  the METHOD';
  const { norm, map } = normaliseForMatch(src);
  check('归一化：折叠空白/连字符/大小写', norm === 'dependenciesofthemethod', norm);
  check('归一化：索引映射回原串', map.length === norm.length && map.every((i) => i >= 0 && i < src.length));
}

/** 核心场景：索引里已被接回的断词，要在带连字符的 text layer 里找到。 */
{
  const indexed = 'We measured cross-page dependencies in the corpus.';
  const layer = 'We measured cross-page depen-\ndencies in the corpus.';
  const needles = quoteNeedles(indexed);
  check('断词（真实连字符）：命中', matchStrength(layer, needles) === 3, String(matchStrength(layer, needles)));
  const hit = findQuote(layer, needles);
  // 命中区间是「原始」文本里的跨度：跨了断行，因此原文里含 "depen-" 与 "dencies"，
  // 高亮框正是靠这个区间逐字符映射回去的。
  const span = hit ? layer.slice(hit.start, hit.end) : '';
  check(
    '断词：命中区间覆盖整段原文（含连字符与换行）',
    span.includes('depen-') && span.includes('dencies') && span.startsWith('We measured'),
    JSON.stringify(span)
  );
}

/** pdfium 把断字报成 U+0002，text layer 里可能是这个控制符。 */
{
  const needles = quoteNeedles('benchmarking of neural retrieval systems');
  check('U+0002 断字符：命中', matchStrength('bench\u0002marking of neural retrieval systems', needles) === 3);
  check('U+00AD 软连字符：命中', matchStrength('bench\u00ADmarking of neural retrieval systems', needles) === 3);
  check('U+2010 排版连字符：命中', matchStrength('bench\u2010marking of neural retrieval systems', needles) === 3);
}

/** 双栏排版 / 硬换行 / 大小写差异。 */
{
  const needles = quoteNeedles('Accuracy improved to 91.3% over all three baselines');
  const messy = 'ACCURACY improved to\n91.3%   over all three\nbaselines in every setting';
  check('换行+空格+大小写：命中', matchStrength(messy, needles) >= 2, String(matchStrength(messy, needles)));
}

/** 长引用尾部被页眉/页脚/脚注打断：退化为句首仍要能定位。 */
{
  const needles = quoteNeedles(
    'Our approach scales to large repositories without any additional annotation effort at all'
  );
  const broken = 'Our approach scales to large repositories 12 Journal of Systems Research without any other text';
  const strength = matchStrength(broken, needles);
  check('长引用被页眉打断：退化到句首', strength === 2 || strength === 1, String(strength));
  const hit = findQuote(broken, needles);
  check('退化命中仍落在正确句首', Boolean(hit) && broken.slice(hit.start, hit.start + 10) === 'Our approa', hit && broken.slice(hit.start, hit.start + 10));
}

/** 中文引用：分块器不在汉字间留空格，text layer 可能留。 */
{
  const needles = quoteNeedles('该方法在多个数据集上取得最优结果');
  check('中文：空格差异下命中', matchStrength('该方法在多个 数据集上取得 最优结果', needles) === 3);
}

/** 找不到就明确返回 null（调用方会退化为只跳页，而不是乱高亮）。 */
{
  const needles = quoteNeedles('这一句根本不在这一页上出现');
  check('无关页面：不误判', matchStrength('完全不同的另一段文字', needles) === 0);
  check('无关页面：findQuote 返回 null', findQuote('完全不同的另一段文字', needles) === null);
  check('空引用：不炸也不误判', quoteNeedles('   ').length === 0 && findQuote('任意', quoteNeedles('   ')) === null);
}

/** 与旧行为对比：原来的「只去空白」匹配在这份 text layer 上是失败的。 */
{
  const indexed = 'We measured cross-page dependencies in the corpus.';
  const layer = 'We measured cross-page depen-\ndencies in the corpus.';
  const oldNeedle = indexed.replace(/\s+/g, '');
  const oldFlat = layer.replace(/\s+/g, '');
  check('旧实现：同一段确实匹配不上（回归根源）', !oldFlat.includes(oldNeedle));
  check('新实现：同一段匹配成功', matchStrength(layer, quoteNeedles(indexed)) === 3);
}

console.log(failed === 0 ? '\n✅ 引用定位（高亮）全部通过' : `\n❌ ${failed} 项失败`);
process.exit(failed === 0 ? 0 : 1);
