import { openInBrowser, routeLink } from '@/lib/externalLinks';

/** Anchor that opens http(s) links in the system browser instead of navigating
 *  the app's webview. Other links (anchors, mailto, ...) render normally. */
export function ExternalLink(props: React.AnchorHTMLAttributes<HTMLAnchorElement>) {
  const { href } = props;
  if (href && routeLink(href) === 'external') {
    return (
      <a
        {...props}
        href={href}
        onClick={(e) => {
          e.preventDefault();
          openInBrowser(href);
        }}
        className="text-primary hover:underline"
      />
    );
  }
  return <a {...props} className="text-primary hover:underline" />;
}
