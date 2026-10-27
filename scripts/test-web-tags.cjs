/*
 * KYB (Know Your Business) — tag filtering and form regressions
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const elements = new Map();
function el(id, value = '') {
  if (!elements.has(id)) elements.set(id, {value, innerHTML: '', textContent: '', classList: {toggle() {}}, getAttribute() {}});
  return elements.get(id);
}
const ctx = vm.createContext({
  console, URLSearchParams, setTimeout, clearTimeout, setInterval: () => 0,
  document: {getElementById: id => el(id), querySelectorAll: () => [], addEventListener() {}},
  window: {location: {hash: ''}},
  history: {pushState(_a, _b, hash) { ctx.window.location.hash = hash; }},
});
const run = code => vm.runInContext(code, ctx);
for (const file of ['00-core.js', '10-knowledge.js', '45-deadlines.js', '70-modals.js', '80-router.js']) {
  run(fs.readFileSync(path.join(__dirname, '../src/web/js', file), 'utf8'));
}
const app = run('App');
const calls = [];
const tagged = [
  ...Array.from({length: 101}, (_, i) => ({key: `note-${i}`, kind: 'knowledge', tags: ['hyperliquid']})),
  {key: 'inc-tagged', kind: 'incident', tags: ['hyperliquid']},
  {key: 'task-tagged', kind: 'task', tags: ['hyperliquid']},
];
ctx.apiFetch = async url => {
  calls.push(url);
  if (url.startsWith('/search?')) return {hits: url.includes('tag=hyperliquid') ? tagged : [{key: 'unrelated', kind: 'knowledge', tags: []}]};
  return {};
};
for (const name of ['renderKnowledgeList', 'renderIncidents', 'renderTasks', 'renderHeaderStats', 'renderDeadlineStrip', 'renderFeed', 'renderPlaces', 'updateLastSyncTicker', 'showToast']) ctx[name] = () => {};
ctx.loadWhoami = async () => {};
ctx.meName = () => 'owner';
ctx.refuseReadOnly = () => false;
ctx.openModal = () => {};
ctx.markRequired = () => true;
ctx.apiPost = async (url, payload) => { ctx.posted = {url, payload}; return {ok: false}; };

(async () => {
  app.entries = [{key: 'unrelated', kind: 'knowledge', tags: []}];
  run("filterByTag('hyperliquid')");
  await new Promise(resolve => setImmediate(resolve));
  assert(calls.includes('/search?tag=hyperliquid&limit=1000'));
  assert.equal(run('getFilteredEntries().length'), 103);
  for (const kind of ['knowledge', 'incident', 'task']) {
    app.currentFilter = kind;
    assert.equal(run('getFilteredEntries().length'), kind === 'knowledge' ? 101 : 1);
  }
  app.currentFilter = 'all';
  await ctx.refreshAll();
  assert.equal(run('getFilteredEntries().length'), 103, 'poll keeps tag results');
  app.tagFilter = null;
  await ctx.refreshAll();
  assert.equal(app.entries[0].key, 'unrelated', 'clearing tag restores ordinary listing');

  let finish;
  ctx.apiFetch = async url => url.startsWith('/search?') ? new Promise(resolve => {finish = resolve;}) : {};
  app.tagFilter = 'old-tag';
  const pending = ctx.refreshAll();
  app.tagFilter = 'new-tag';
  finish({hits: tagged});
  await pending;
  assert.equal(app.entries[0].key, 'unrelated', 'late response cannot replace a newer tag');

  for (const kind of ['incident', 'task']) {
    run(`switchNewModalType('${kind}')`);
    assert(el('newModalBody').innerHTML.includes('id="newTags"'), `${kind} creation has tag field`);
    for (const [id, value] of Object.entries({newKey: `${kind}-tagged`, newTitle: 'Title', newBody: 'Body', newTags: 'hyperliquid, replay', newService: 'hl-node', newSeverity: 'low', newDetection: 'health check'})) el(id).value = value;
    await ctx.submitNewModal();
    assert.equal(JSON.stringify(ctx.posted.payload.tags), '["hyperliquid","replay"]');
    const existing = {key: `${kind}-tagged`, kind, title: 'Title', body: 'Body', status: kind === 'task' ? 'blocked' : 'mitigated', blocked_reason: 'Waiting', tags: ['old'], knowledge: ['linked'], refs: ['reference'], resolution: 'Workaround'};
    ctx.apiFetch = async () => existing;
    await ctx.openEditModal(existing.key);
    assert(el('editModalBody').innerHTML.includes('id="editTags"'), `${kind} edit has tag field`);
    for (const [id, value] of Object.entries({editKey: existing.key, editTitle: 'Title', editBody: 'Body', editTags: 'hyperliquid, replay', editService: 'hl-node', editSeverity: 'low'})) el(id).value = value;
    await ctx.submitEditModal();
    assert.equal(JSON.stringify(ctx.posted.payload.tags), '["hyperliquid","replay"]');
    for (const field of ['status', 'knowledge', 'refs', 'resolution']) assert.deepEqual(ctx.posted.payload[field], existing[field], `${kind} preserves ${field}`);
  }
  console.log('PASS: tag server lookup, >100 results, every kind, poll, stale response, create/edit tags and metadata');
})().catch(error => { console.error(error); process.exitCode = 1; });
