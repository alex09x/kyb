/*
 * KYB (Know Your Business) — git-backed knowledge base and incident tracker for AI agent fleets
 * Copyright (c) 2026 Alexander Panasenko
 *
 * Contact: alex@prod.codes
 * Author: https://prod.codes/about/
 * Project: https://github.com/alex09x/kyb
 * SPDX-License-Identifier: MIT
 */

    // Relations are what the entries themselves say - a typed field or an internal link -
    // and every row names how it got here. Nothing is inferred from tags.
    const VIA_WORDS = { knowledge: 'cited', parent_task: 'parent task', link: 'linked in the text', service: 'names this service', host: 'names this host' };

    function relRow(r, because) {
      if (r.missing) return `<div class="rel-row missing"><span class="mono">${escapeHtml(r.key)}</span><span class="why">${VIA_WORDS[r.via] || r.via} · not in the base</span></div>`;
      const state = r.kind === 'incident' ? (r.live ? (r.severity || 'open') : 'resolved') : r.kind === 'task' ? (r.live ? (r.status || 'open').replace('_', ' ') : 'done') : '';
      const tone = !r.live ? (r.kind === 'knowledge' ? '' : 'ok') : r.kind === 'incident' ? (r.severity === 'critical' ? 'crit' : 'amber') : r.status === 'blocked' ? 'amber' : 'acc';
      return `<a class="rel-row" href="#/knowledge/${encodeURIComponent(r.key)}" onclick="handleCardClick(event, ${jsArg(r.key)}, 'knowledge')">
        <span class="k">${r.kind === 'knowledge' ? 'note' : r.kind}</span>
        <span class="t"><b>${escapeHtml(r.title || r.key)}</b><span class="mono">${escapeHtml(r.key)}</span></span>
        ${state ? `<span class="pill ${tone}">${escapeHtml(state)}</span>` : ''}
        <span class="why">${because && because.length ? `because of ${because.map(escapeHtml).join(', ')}` : (VIA_WORDS[r.via] || escapeHtml(r.via))}</span>
      </a>`;
    }

    // ---- Related, in the card
    function relatedHtml(key) {
      const rel = App.related.get(key);
      if (!rel || (!rel.outgoing.length && !rel.incoming.length)) return '';
      const live = rel.incoming.filter(r => r.live && r.kind === 'incident').length;
      return `<div class="rel">
        ${rel.incoming.length ? `<div class="rel-h">Referenced by <span>${rel.incoming.length}${live ? ` · ${live} open incident${live === 1 ? '' : 's'}` : ''}</span></div>${rel.incoming.map(r => relRow(r)).join('')}` : ''}
        ${rel.outgoing.length ? `<div class="rel-h">Refers to <span>${rel.outgoing.length}</span></div>${rel.outgoing.map(r => relRow(r)).join('')}` : ''}
      </div>`;
    }

    async function loadRelated(key) {
      const data = await apiFetch(`/knowledge/${encodeURIComponent(key)}/related`);
      if (!data || !data.outgoing) return;
      const before = JSON.stringify(App.related.get(key) || null);
      App.related.set(key, { outgoing: data.outgoing, incoming: data.incoming });
      const box = document.getElementById('relatedBox');
      // drawn only into the card it was asked for, and only when the answer changed
      if (box && box.dataset.key === key && before !== JSON.stringify(App.related.get(key))) box.innerHTML = relatedHtml(key);
    }

    // ---- Services and hosts: what is affected right now, and why
    const placeHash = p => `#/${p.kind === 'host' ? 'hosts' : 'services'}/${encodeURIComponent(p.name)}`;

    function openPlace(event, kind, name) {
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.button === 1) return;
      event.preventDefault();
      if (App.activeTab !== 'services') switchTab('services', false);
      selectPlace(kind, name, true);
    }

    function selectPlace(kind, name, updateHash) {
      App.place = { kind, name };
      if (updateHash && window.location.hash !== placeHash(App.place)) history.pushState(null, '', placeHash(App.place));
      return renderPlaces();
    }

    async function loadImpact() {
      const { kind, name } = App.place;
      const data = await apiFetch(`/impact?${kind}=${encodeURIComponent(name)}`);
      // another place was picked while this one was loading
      if (App.place.kind !== kind || App.place.name !== name) return;
      App.impact = data;
      renderPlaceDetail();
    }

    function placeRow(kind, p) {
      const on = App.place && App.place.kind === kind && App.place.name === p.name;
      const tone = p.worst === 'critical' ? 'crit' : p.open ? 'amber' : '';
      return `<a class="place ${on ? 'on' : ''}" href="${placeHash({ kind, name: p.name })}" onclick="openPlace(event, '${kind}', ${jsArg(p.name)})">
        <span class="mono">${escapeHtml(p.name)}</span>
        ${p.open ? `<span class="pill ${tone}">${p.open} open</span>` : `<span class="quiet">${p.total} closed</span>`}
      </a>`;
    }

    function renderPlaces() {
      const badge = document.getElementById('badgeServicesCount');
      const burning = App.places.services.filter(s => s.open).length;
      if (badge) { badge.textContent = burning; badge.className = burning ? 'tab-badge red' : 'tab-badge'; }
      const list = document.getElementById('placesList');
      if (!list || App.activeTab !== 'services') return Promise.resolve();
      const { services, hosts } = App.places;
      if (!services.length && !hosts.length) {
        list.innerHTML = `<div class="inc-empty">No incident names a service or a host yet.</div>`;
        document.getElementById('placeDetail').innerHTML = '';
        return Promise.resolve();
      }
      // nothing picked yet: open the worst one, without touching the address
      if (!App.place) App.place = services.length ? { kind: 'service', name: services[0].name } : { kind: 'host', name: hosts[0].name };
      list.innerHTML = `
        <div class="rel-h">Services <span>${services.length}</span></div>${services.map(s => placeRow('service', s)).join('')}
        ${hosts.length ? `<div class="rel-h">Hosts <span>${hosts.length}</span></div>${hosts.map(h => placeRow('host', h)).join('')}` : ''}`;
      return loadImpact();
    }

    function renderPlaceDetail() {
      const el = document.getElementById('placeDetail');
      const d = App.impact;
      if (!el) return;
      if (!d) { el.innerHTML = `<div class="inc-empty">Could not load ${escapeHtml(App.place.name)}.</div>`; return; }
      const section = (title, rows, empty) => `<div class="rel-h">${title} <span>${rows.length}</span></div>${rows.length ? rows.map(r => relRow(r, r.because)).join('') : `<div class="rel-none">${empty}</div>`}`;
      el.innerHTML = `
        <div class="dh-kind"><span class="pill">${d.kind}</span></div>
        <h1 class="mono">${escapeHtml(d.name)}</h1>
        <div class="place-sum ${d.open ? 'hot' : ''}">${d.open ? `${d.open} open incident${d.open === 1 ? '' : 's'} right now` : 'Nothing open right now'}</div>
        ${section('Incidents', d.incidents, 'No incident names it.')}
        ${section('Runbooks and notes', d.runbooks, 'No incident here cites a note. The next one should.')}
        ${section('Open tasks', d.tasks, 'No open task points at these incidents or notes.')}`;
    }
