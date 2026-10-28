# Marketing homepage

`site/index.html` is the marketing entry point; the existing documentation routes remain in place.
It is plain HTML, CSS (`home.css`) and progressive enhancement (`home.js`). There is no dependency
installation or build step. Existing shared `app.js` retains the site's pageview integration.

## Content and assets

- Product scope and free/open-source positioning: `README.md`, `LICENSE`, `PHILOSOPHY.md`.
- Commands and action safeguards: `docs/CLI_CONTRACT.md`, `docs/OPERATIONS_GUIDE.md`,
  `crates/system/src/lib.rs`, `crates/ui/src/lib.rs`.
- Platform and package statements: `docs/COMPATIBILITY.md`, `docs/INSTALLING.md`.
- Screenshot: user-supplied live `lens-top` capture on `linux-build`, supplied 28 October 2026.
  `assets/lens-top-live.webp` preserves the full capture; `lens-top-preview.webp` is a top crop.
  Both use WebP compression; their displayed data has not been invented or replaced.
- `assets/lens-social.png` is a share card composed from the real capture and homepage typography.

The homepage deliberately qualifies telemetry claims as applying to the **Lens application**.
Platform-specific data remains subject to the compatibility matrix and local permissions.
The install CTA uses `releases/latest` so release versions do not become stale.

## Check and publish

```sh
python3 scripts/check-pages.py site
for script in site/*.js; do node --check "$script"; done
python3 -m http.server 8765 --directory site
```

Check narrow and desktop layouts, visible keyboard focus, install anchors and copy-command feedback.
The page remains navigable and commands remain selectable without JavaScript.

The existing Pages workflow validates pull requests and deploys `site/` after changes reach `main`.
It checks every top-level JavaScript file, including the homepage enhancement.

At inspection on 28 October 2026, GitHub Pages was already configured with custom domain
`lens.dataplicity.com`, an approved certificate and enforced HTTPS; the domain returned HTTP 200.
This change preserves `site/CNAME` and does not change DNS or Pages domain settings.
For a future recreation, configure Pages to use GitHub Actions, point the `lens` CNAME record at
`wildfoundry.github.io`, set the custom domain in Pages and wait for HTTPS issuance. Do not create a
second record while the existing domain configuration is working.
