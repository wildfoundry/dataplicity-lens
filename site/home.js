(() => {
  const button = document.querySelector('[data-copy-install]');
  const code = document.querySelector('#brew-commands');
  const status = document.querySelector('.copy-status');
  button?.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(code.textContent.trim());
      status.textContent = 'Commands copied.';
    } catch {
      const selection = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents(code);
      selection.removeAllRanges();
      selection.addRange(range);
      status.textContent = 'Commands selected. Copy with your usual keyboard shortcut.';
    }
  });
})();
