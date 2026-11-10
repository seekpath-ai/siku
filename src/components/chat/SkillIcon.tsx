/** 技能图标：办公三件套用品牌色字母瓷贴（W=Word 蓝 / X=Excel 绿 /
 *  P=PowerPoint 橙红），其余技能用名称首字母瓷贴（颜色按名称哈希从
 *  柔和色板中稳定选取）。统一圆角方块+单字母，辨识度与版权风险兼顾。 */

const OFFICE_ICONS: Record<string, { letter: string; bg: string; label: string }> = {
  docx: { letter: 'W', bg: '#2B579A', label: 'Word 技能' },
  xlsx: { letter: 'X', bg: '#217346', label: 'Excel 技能' },
  pptx: { letter: 'P', bg: '#D24726', label: 'PowerPoint 技能' },
};

/** 通用技能的柔和色板（与办公品牌色明度相近，混排不突兀）。 */
const PALETTE = [
  '#5B6ABF', // 蓝紫
  '#0E7490', // 青
  '#B45309', // 琥珀
  '#BE185D', // 品红
  '#4D7C0F', // 橄榄绿
  '#6D28D9', // 紫
  '#0F766E', // 墨绿
  '#9F1239', // 绯红
];

function tileColor(name: string): string {
  let h = 0;
  for (const c of name) h = (h * 31 + c.codePointAt(0)!) >>> 0;
  return PALETTE[h % PALETTE.length];
}

export function SkillIcon({ name, size = 16 }: { name: string; size?: number }) {
  const office = OFFICE_ICONS[name];
  const letter = (office?.letter ?? name.trim().charAt(0).toUpperCase()) || '?';
  const bg = office?.bg ?? tileColor(name);
  return (
    <span
      role="img"
      aria-label={office?.label ?? `技能 ${name}`}
      title={office?.label ?? name}
      className="inline-flex items-center justify-center rounded-[3px] text-white font-semibold select-none shrink-0"
      style={{
        width: size,
        height: size,
        backgroundColor: bg,
        fontSize: size * 0.68,
        lineHeight: 1,
      }}
    >
      {letter}
    </span>
  );
}
