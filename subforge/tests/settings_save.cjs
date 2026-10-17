const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.listeners = {}; this.classList = { toggle() {} }; this.attributes = {}; }
  append(...items) { this.children.push(...items); }
  setAttribute(key, value) { this.attributes[key] = value; }
  addEventListener(name, fn) { this.listeners[name] = fn; }
}
const body = new Element('body');
const windowListeners = {};
let confirmValue = false;
const context = {
  document: { body, documentElement: new Element('html'), createElement: tag => new Element(tag) },
  window: { confirm: () => confirmValue, addEventListener: (name, fn) => { windowListeners[name] = fn; } },
};
vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../web/settings-save.js'), 'utf8'), context);
const create = context.window.SubforgeSettingsSave.create;

async function main() {
  let dirtyA = true, dirtyB = true;
  let failB = false;
  const controller = create({ groups: {
    a: { isDirty: () => dirtyA, save: async () => { dirtyA = false; }, discard: () => { dirtyA = false; } },
    b: { isDirty: () => dirtyB, save: async () => { if (failB) throw Error('save failed'); dirtyB = false; }, discard: () => { dirtyB = false; } },
  } });
  assert.equal(controller.isDirty(), true);
  assert.equal(await controller.save(), true);
  assert.equal(controller.isDirty(), false);

  dirtyA = true; dirtyB = true; failB = true;
  assert.equal(await controller.save(), false);
  assert.equal(dirtyA, false, 'successful group remains saved');
  assert.equal(dirtyB, true, 'failed group remains dirty');

  confirmValue = false;
  assert.equal(controller.guardNavigation(), false);
  assert.equal(dirtyB, true, 'cancelled navigation preserves edits');
  confirmValue = true;
  assert.equal(controller.guardNavigation(), true);
  assert.equal(controller.isDirty(), false, 'accepted navigation discards synchronously');

  dirtyA = true;
  const discardButton = body.children.at(-1).children[1];
  await discardButton.listeners.click();
  assert.equal(controller.isDirty(), false, 'discard button restores saved state without a second prompt');

  dirtyA = true;
  let prevented = false;
  windowListeners.beforeunload({ preventDefault() { prevented = true; }, set returnValue(value) { this.value = value; } });
  assert.equal(prevented, true);
  assert.equal(controller.isDirty(), true);
}

main().catch(error => { process.nextTick(() => { throw error; }); });
