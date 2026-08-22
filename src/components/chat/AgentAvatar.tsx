interface Props {
  name: string;
  color?: string | null;
  size?: number;
  className?: string;
}

const AVATAR_COLORS = [
  '#ef4444', '#f97316', '#f59e0b', '#84cc16', '#22c55e',
  '#14b8a6', '#06b6d4', '#3b82f6', '#6366f1', '#8b5cf6',
  '#a855f7', '#d946ef', '#ec4899', '#f43f5e',
];

export function getAvatarColor(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i++) {
    hash = name.charCodeAt(i) + ((hash << 5) - hash);
  }
  const index = Math.abs(hash) % AVATAR_COLORS.length;
  return AVATAR_COLORS[index];
}

export function getAvatarInitial(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return '?';
  const char = trimmed.charAt(0);
  return /[\u4e00-\u9fa5]/.test(char) ? char : char.toUpperCase();
}

export function AgentAvatar({ name, color, size = 28, className = '' }: Props) {
  const bg = color || getAvatarColor(name);
  const initial = getAvatarInitial(name);

  return (
    <span
      className={`inline-flex items-center justify-center rounded-full font-semibold text-white shrink-0 ${className}`}
      style={{
        width: size,
        height: size,
        fontSize: size * 0.45,
        backgroundColor: bg,
      }}
      title={name}
    >
      {initial}
    </span>
  );
}
