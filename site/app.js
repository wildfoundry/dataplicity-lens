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
})();
