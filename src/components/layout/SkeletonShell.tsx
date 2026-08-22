/**
 * Skeleton screen shown briefly after the splash fades out.
 * Covers only the sidebar + content area (TitleBar and TabBar
 * are already rendered by AppShell). The 200ms transition gives
 * the router time to commit the first route and prevents a
 * flash of unstyled layout.
 */
export function SkeletonShell() {
  return (
    <div
      style={{
        display: 'flex',
        flex: 1,
        height: '100%',
        overflow: 'hidden',
        background: '#1A1A1E',
      }}
    >
      {/* Sidebar skeleton */}
      <div
        style={{
          width: 56,
          background: '#1E1E24',
          padding: 12,
          display: 'flex',
          flexDirection: 'column',
          gap: 6,
          flexShrink: 0,
        }}
      >
        {[0.8, 0.5, 0.9, 0.6, 0.75, 0.45, 0.85, 0.55].map((w, i) => (
          <div
            key={i}
            style={{
              width: `${w * 100}%`,
              height: 20,
              borderRadius: 4,
              background: '#2E2E36',
            }}
          />
        ))}
      </div>

      {/* Content area skeleton */}
      <div
        style={{
          flex: 1,
          padding: 24,
          display: 'flex',
          flexDirection: 'column',
          gap: 12,
        }}
      >
        <div
          style={{
            width: '40%',
            height: 28,
            borderRadius: 4,
            background: '#2E2E36',
          }}
        />
        <div
          style={{
            width: '60%',
            height: 16,
            borderRadius: 4,
            background: '#2E2E36',
          }}
        />
        <div style={{ height: 24 }} />
        {[0.95, 0.7, 0.85, 0.6, 0.9].map((w, i) => (
          <div
            key={i}
            style={{
              width: `${w * 100}%`,
              height: 48,
              borderRadius: 8,
              background: '#24242B',
            }}
          />
        ))}
      </div>
    </div>
  );
}
