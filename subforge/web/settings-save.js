/* Shared save bar for settings, schedules, and worker allocation. */
(() => {
  'use strict';

  const create = ({ groups, onStateChange = () => {}, confirmDiscard, onError = () => {} } = {}) => {
    const entries = Object.entries(groups || {});
    let saving = false;
    const dirtyEntries = () => entries.filter(([, group]) => group.isDirty());
    const dirty = () => dirtyEntries().length > 0;

    const bar = document.createElement('aside');
    bar.className = 'settings-save-bar';
    bar.setAttribute('aria-live', 'polite');
    bar.hidden = true;
    const message = document.createElement('span');
    message.className = 'settings-save-message';
    message.textContent = 'Unsaved changes';
    const discard = document.createElement('button');
    discard.type = 'button'; discard.className = 'button button-secondary'; discard.textContent = 'Discard';
    const save = document.createElement('button');
    save.type = 'button'; save.className = 'button button-primary'; save.textContent = 'Save';
    bar.append(message, discard, save);
    document.body.append(bar);

    const render = () => {
      const hasDirty = dirty();
      bar.hidden = !hasDirty && !saving;
      save.disabled = saving || !hasDirty;
      discard.disabled = saving || !hasDirty;
      message.textContent = saving ? 'Saving changes…' : hasDirty ? 'Unsaved changes' : 'Changes saved';
      document.documentElement.classList.toggle('has-unsaved-changes', hasDirty);
      onStateChange({ dirty: hasDirty, saving, groups: dirtyEntries().map(([name]) => name) });
    };

    const saveAll = async () => {
      if (saving || !dirty()) return false;
      saving = true; render();
      const pending = dirtyEntries();
      const results = await Promise.allSettled(pending.map(([, group]) => group.save()));
      saving = false;
      const failed = results.some(result => result.status === 'rejected');
      render();
      return !failed && !dirty();
    };

    const discardAll = async () => {
      if (saving || !dirty()) return;
      if (confirmDiscard && !(await confirmDiscard())) return;
      try {
        for (const [, group] of dirtyEntries()) await group.discard();
      } catch (error) { onError(error); }
      render();
    };

    const guardNavigation = () => {
      if (saving) return false;
      if (!dirty()) return true;
      const accept = window.confirm('You have unsaved changes. Leave this page and discard them?');
      if (!accept) return false;
      try {
        for (const [, group] of dirtyEntries()) {
          const result = group.discard();
          if (result && typeof result.then === 'function') throw new Error('Navigation discard callbacks must be synchronous.');
        }
      } catch (error) { onError(error); render(); return false; }
      render();
      return true;
    };

    save.addEventListener('click', () => { void saveAll(); });
    discard.addEventListener('click', () => { void discardAll(); });
    window.addEventListener('beforeunload', event => {
      if (!dirty() && !saving) return;
      event.preventDefault(); event.returnValue = '';
    });

    return { render, save: saveAll, discard: discardAll, guardNavigation, isDirty: dirty };
  };

  window.SubforgeSettingsSave = { create };
})();
