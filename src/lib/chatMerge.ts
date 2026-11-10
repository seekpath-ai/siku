import type { ChatMessage } from '@/lib/types';

/** 本地乐观气泡的 id 前缀（DB 行的 uuid 永不以这些开头）。 */
export const isTempMessageId = (id: string) =>
  id.startsWith('user_') || id.startsWith('error_') || id.startsWith('assistant_');

export interface MergeResult {
  merged: ChatMessage[];
  /** DB 消息 id → 沿用的临时 id（步骤等按 message_id 关联的数据要随它改挂）。 */
  twinTempByDbId: Map<string, string>;
}

/** 把 DB 拉取的（部分）历史并入当前消息列表。
 *
 * 规则：
 * - 已持久化的行原样保留；
 * - 页里的新行按 DB 顺序追加；与临时气泡内容一致的助手行在原位换用临时
 *   id（静默替换 —— id 是 React key，换 id 会让气泡卸载重挂、markdown 重
 *   渲染，是 done 时闪跳的主因）；用户行不做替换（纯文本重挂载无感知）；
 * - 没有 DB 孪生、内容也未出现在列表里的临时消息保留在队尾：任何一条
 *   消息都不能因为增量页缺口而凭空消失。内容已存在的临时消息视为孪生
 *   已并入（如 chat:message_updated 的整表替换竞态），丢弃以去重。
 */
export function mergeMessages(current: ChatMessage[], pageRows: ChatMessage[]): MergeResult {
  const persisted = current.filter((m) => !isTempMessageId(m.id));
  const known = new Set(persisted.map((m) => m.id));
  const tempMessages = current.filter((m) => isTempMessageId(m.id));
  const usedTempIds = new Set<string>();
  const twinTempByDbId = new Map<string, string>();

  const fresh = pageRows
    .filter((m) => !known.has(m.id))
    .map((m) => {
      const twin = tempMessages.find(
        (t) => !usedTempIds.has(t.id) && t.role === m.role && t.content === m.content,
      );
      if (!twin) return m;
      usedTempIds.add(twin.id);
      if (m.role === 'assistant') {
        twinTempByDbId.set(m.id, twin.id);
        return twin;
      }
      return m;
    });

  const base = [...persisted, ...fresh];
  const orphans = tempMessages.filter(
    (t) =>
      !usedTempIds.has(t.id) &&
      !base.some((m) => m.role === t.role && m.content === t.content),
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
  // 孪生替换打破「fresh 必新于 persisted」的假设：回复的 DB 行因临时孪生
  // 占位落在 fresh，而更晚的用户行可能已被 chat:message_updated 的合并抢
  // 先并入 persisted —— 直接拼接会把回复排到下一轮提问之后。统一按
  // created_at 稳定排序归位（同时间戳保持现有相对顺序）。
  const merged = [...base, ...orphans];
  merged.sort((a, b) => (a.created_at < b.created_at ? -1 : a.created_at > b.created_at ? 1 : 0));
  return { merged, twinTempByDbId };
}
