import { artworkColor, artworkIcon } from './artifactMeta';

export function ArtifactTypeIcon({
  ext,
  size = 13,
  className = '',
}: {
  ext: string;
  size?: number;
  className?: string;
}) {
  const Icon = artworkIcon(ext);
  return (
    <Icon
      size={size}
      className={`shrink-0 ${artworkColor(ext)} ${className}`.trim()}
      aria-hidden="true"
    />
  );
}
