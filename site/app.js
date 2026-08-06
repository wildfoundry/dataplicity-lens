(() => {
  const root = document.documentElement;
  const stored = localStorage.getItem('lens-theme');
  const preferred = window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  root.dataset.theme = stored || preferred;

  const themeButton = document.querySelector('[data-theme-toggle]');
  const updateThemeLabel = () => {
    if (!themeButton) return;
    const next = root.dataset.theme === 'dark' ? 'light' : 'dark';
    themeButton.setAttribute('aria-label', `Use ${next} theme`);
    themeButton.innerHTML = root.dataset.theme === 'dark'
      ? '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v2m0 14v2M3 12h2m14 0h2M5.64 5.64l1.42 1.42m9.88 9.88 1.42 1.42m0-12.72-1.42 1.42M7.06 16.94l-1.42 1.42M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>'
      : '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20 15.3A8.4 8.4 0 0 1 8.7 4a8.4 8.4 0 1 0 11.3 11.3Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/></svg>';
  };
  updateThemeLabel();
  themeButton?.addEventListener('click', () => {
    root.dataset.theme = root.dataset.theme === 'dark' ? 'light' : 'dark';
    localStorage.setItem('lens-theme', root.dataset.theme);
    updateThemeLabel();
  });

  const menuButton = document.querySelector('[data-menu-toggle]');
  const nav = document.querySelector('[data-nav]');
  menuButton?.addEventListener('click', () => {
    const open = nav.classList.toggle('is-open');
    menuButton.setAttribute('aria-expanded', String(open));
  });
  nav?.addEventListener('click', event => {
    if (event.target.closest('a')) {
      nav.classList.remove('is-open');
      menuButton?.setAttribute('aria-expanded', 'false');
    }
  });

  const header = document.querySelector('.site-header');
  const setScrolled = () => header?.classList.toggle('is-scrolled', window.scrollY > 8);
  setScrolled();
  window.addEventListener('scroll', setScrolled, { passive: true });

  document.querySelectorAll('[data-copy]').forEach(button => {
    button.addEventListener('click', async () => {
      const block = button.closest('.code-block')?.querySelector('code');
      if (!block) return;
      const original = button.textContent;
      try {
        await navigator.clipboard.writeText(block.textContent.trim());
        button.textContent = 'Copied';
      } catch {
        button.textContent = 'Select to copy';
      }
      window.setTimeout(() => { button.textContent = original; }, 1600);
    });
  });

  const VISITOR_STORAGE_KEY = 'dp-lens-visitor-id';
  const USAGE_ENDPOINT = 'https://api.dataplicity.com/api/public/lens-usage/';

  function prefersNoTracking() {
    try {
      const nav = navigator;
      const flag = nav.doNotTrack || nav.msDoNotTrack || window.doNotTrack;
      return flag === '1' || flag === 'yes';
    } catch {
      return false;
    }
  }

  function getVisitorId() {
    try {
      const existing = window.localStorage.getItem(VISITOR_STORAGE_KEY);
      if (existing && /^[A-Za-z0-9_-]+$/.test(existing) && existing.length <= 64) {
        return existing;
      }
      const generated = typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
        ? crypto.randomUUID().replace(/-/g, '')
        : `v${Date.now().toString(36)}${Math.random().toString(36).slice(2, 12)}`;
      window.localStorage.setItem(VISITOR_STORAGE_KEY, generated);
      return generated;
    } catch {
      return null;
    }
  }

  function normalizePath(rawPath) {
    try {
      const url = new URL(rawPath, window.location.origin);
      let path = url.pathname || '/';
      if (!path.startsWith('/')) path = `/${path}`;
      path = path.replace(/\/+/g, '/');
      if (path.length > 1) path = path.replace(/\/$/, '');
      const lowered = path.toLowerCase();
      if (
        lowered.startsWith('/assets/') ||
        lowered.startsWith('/robots') ||
        lowered.startsWith('/sitemap') ||
        lowered.startsWith('/favicon')
      ) {
        return null;
      }
      if (/\.(?:js|css|map|png|jpe?g|gif|svg|ico|webp|woff2?|ttf|eot|txt|json)$/i.test(path)) {
        return null;
      }
      if (lowered.endsWith('.html')) {
        path = path.slice(0, -5) || '/';
      }
      if (path === '/index') path = '/';
      return path;
    } catch {
      return null;
    }
  }

  function trackLensPageview(rawPath = window.location.pathname) {
    if (prefersNoTracking()) return;
    const path = normalizePath(rawPath);
    const visitorId = getVisitorId();
    if (!path || !visitorId) return;
    const body = JSON.stringify({
      path,
      visitor_id: visitorId,
      referrer: document.referrer || '',
      ts: new Date().toISOString(),
    });
    try {
      if (typeof navigator.sendBeacon === 'function') {
        const blob = new Blob([body], { type: 'application/json' });
        if (navigator.sendBeacon(USAGE_ENDPOINT, blob)) return;
      }
    } catch {
      // fall through
    }
    try {
      void fetch(USAGE_ENDPOINT, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body,
        mode: 'cors',
        credentials: 'omit',
        keepalive: true,
      });
    } catch {
      // Best-effort telemetry must never break browsing.
    }
  }

  trackLensPageview();
})();
