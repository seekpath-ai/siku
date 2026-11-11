import type { ChatMessage } from '@/lib/types';

/** 本地乐观气泡的 id 前缀（DB 行的 uuid 永不以这些开头）。正常路径下
 * 气泡在发送返回 / done 时已换成权威 id，临时 id 只剩持久化失败的兜底。 */
export const isTempMessageId = (id: string) =>
  id.startsWith('user_') || id.startsWith('error_') || id.startsWith('assistant_');

/** 把 DB 拉取的（部分）历史并入当前消息列表，按 id 取并集。
 *
 * 规则：
 * - 已持久化的行原样保留（含发送返回 / done 时已换上权威 id 的气泡，
 *   页里的同一行因此被跳过，不会重复也不会重挂载）；
 * - 页里的新行按 DB 顺序并入；
 * - 没有 DB 对应行、内容也未出现在列表里的临时消息保留兜底：任何一条
 *   消息都不能因为增量页缺口而凭空消失。内容已存在的临时消息视为同一
 *   行已并入（如发送返回前的抢先对齐），丢弃以去重；
 * - 最终按 created_at 稳定排序归位（增量页的新行不一定晚于全部现有行：
 *   远程回合的回复可能与本地刚并入的下一轮提问交错）。
 */
export function mergeMessages(current: ChatMessage[], pageRows: ChatMessage[]): ChatMessage[] {
  const persisted = current.filter((m) => !isTempMessageId(m.id));
  const known = new Set(persisted.map((m) => m.id));
  const fresh = pageRows.filter((m) => !known.has(m.id));

  const base = [...persisted, ...fresh];
  const orphans = current.filter(
    (m) => isTempMessageId(m.id) && !base.some((b) => b.role === m.role && b.content === m.content),
  );
  if (orphans.some((t) => t.role === 'user')) {
    // 用户消息的 DB 行应当在页里（发送即持久化）。走到这里说明游标或
    // 持久化有未知缺口 —— 留下证据供排查。
    console.warn('[chatMerge] 页中未找到用户消息的 DB 行，保留临时气泡', {
      orphans: orphans.map((t) => ({ id: t.id, role: t.role, at: t.created_at })),
      pageFrom: pageRows[0]?.created_at,
      pageCount: pageRows.length,
    });
  }
  const merged = [...base, ...orphans];
  merged.sort((a, b) => (a.created_at < b.created_at ? -1 : a.created_at > b.created_at ? 1 : 0));
  return merged;
}
