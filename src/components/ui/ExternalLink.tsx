import { open } from '@tauri-apps/plugin-shell';

/** Anchor that opens http(s) links in the system browser instead of navigating
 *  the app's webview. Other links (anchors, mailto, ...) render normally. */
export function ExternalLink(props: React.AnchorHTMLAttributes<HTMLAnchorElement>) {
  const { href } = props;
  if (href?.startsWith('http://') || href?.startsWith('https://')) {
    return (
      <a
        {...props}
        href={href}
        onClick={(e) => {
          e.preventDefault();
          open(href).catch(() => {
            // Fallback when running outside Tauri (browser dev/tests).
            window.open(href, '_blank', 'noopener');
          });
        }}
        className="text-primary hover:underline"
      />
    );
  }
  return <a {...props} className="text-primary hover:underline" />;
}
